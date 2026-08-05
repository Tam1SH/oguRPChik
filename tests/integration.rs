//! End-to-end tests over the public API: Endpoint/Listener → handshake →
//! capnp-rpc session, per transport. Unit-level coverage of the pieces
//! lives next to them (net/, auth/, rpc); this file proves the assembled
//! path works the same on every transport.

use testschema::echo_capnp::echo;
use ogurpchik::auth::handshake::{
    ConnectionGate, ConnectionMode, HandshakeMode, authenticate_client, authenticate_server,
    reject_connection,
};
use ogurpchik::endpoint::Endpoint;
use ogurpchik::error::{HandshakeError, RpcError};
use ogurpchik::net::{Conn, Listener};
use ogurpchik::rpc::{RpcSession, Side, spawn_session};
use capnp::capability::Rc;

struct EchoImpl;

impl echo::Server for EchoImpl {
    async fn ping(
        self: Rc<Self>,
        params: echo::PingParams,
        mut results: echo::PingResults,
    ) -> Result<(), capnp::Error> {
        let msg = params.get()?.get_msg()?;
        results
            .get()
            .set_reply(format!("echo {}", msg.to_str()?));
        Ok(())
    }
}

fn hmac() -> HandshakeMode {
    HandshakeMode::hmac(b"integration-secret".to_vec())
}

/// Server: accept one connection, run the handshake, export Echo.
async fn serve_one(listener: &Listener) -> RpcSession<echo::Client> {
    let mut conn = listener.accept().await.expect("accept failed");
    authenticate_server(&mut conn, &hmac())
        .await
        .expect("server handshake failed");
    spawn_session(conn, Side::Server, EchoImpl)
}

/// Client: connect, run the handshake, export Echo (bootstrap is
/// symmetric — the server could call us too).
async fn connect_client(conn: Conn) -> RpcSession<echo::Client> {
    let mut conn = conn;
    authenticate_client(&mut conn, &hmac())
        .await
        .expect("client handshake failed");
    spawn_session(conn, Side::Client, EchoImpl)
}

async fn ping(session: &RpcSession<echo::Client>, msg: &str) -> String {
    let mut req = session.remote().ping_request();
    req.get().set_msg(msg);
    let reply = req.send().promise.await.expect("ping failed");
    reply
        .get()
        .unwrap()
        .get_reply()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

async fn full_stack_ping_pong(listener: Listener, connect: impl std::future::Future<Output = Conn>) {
    let server_task = compio::runtime::spawn(async move {
        let session = serve_one(&listener).await;
        // Hold the session until the client is done, then let it close.
        compio::time::timeout(std::time::Duration::from_secs(5), session.wait())
            .await
            .ok();
    });

    let client_session = connect_client(connect.await).await;
    assert_eq!(ping(&client_session, "ping").await, "echo ping");
    // And back the other way over the same connection (symmetric bootstrap).
    drop(client_session);
    server_task.await.unwrap();
}

#[compio::test]
async fn tcp_full_stack() {
    let listener = Listener::bind_tcp("127.0.0.1:0".parse().unwrap())
        .await
        .expect("bind failed");
    let Listener::Tcp(inner) = &listener else {
        unreachable!()
    };
    let addr = inner.local_addr().unwrap();
    full_stack_ping_pong(listener, async move {
        Conn::connect_tcp(addr).await.expect("connect failed")
    })
    .await;
}

#[compio::test]
async fn uds_full_stack() {
    let path = std::env::temp_dir().join(format!("ogurpchik-it-{}.sock", std::process::id()));
    let listener = Listener::bind_uds(&path).await.expect("bind failed");
    full_stack_ping_pong(listener, async move {
        Conn::connect_uds(&path).await.expect("connect failed")
    })
    .await;
}

#[cfg(windows)]
#[compio::test]
async fn npipe_full_stack() {
    let name = format!("ogurpchik-it-{}", std::process::id());
    let listener = Listener::bind_npipe(&name).await.expect("bind failed");
    full_stack_ping_pong(listener, async move {
        Conn::connect_npipe(&name).await.expect("connect failed")
    })
    .await;
}

#[compio::test]
async fn vsock_loopback_full_stack() {
    const PORT: u32 = 22468;
    let listener = Listener::bind_vsock_loopback(PORT).expect("bind failed");
    full_stack_ping_pong(listener, async move {
        Conn::connect_vsock_loopback(PORT)
            .await
            .expect("connect failed")
    })
    .await;
}

/// A wrong HMAC secret must fail both sides: the server rejects, the client
/// receives the rejection (not a timeout, not a hang).
#[compio::test]
async fn wrong_hmac_is_rejected() {
    let endpoint = Endpoint::Tcp("127.0.0.1:0".parse().unwrap());
    let listener = endpoint.listen().await.expect("listen failed");
    let Listener::Tcp(inner) = &listener else {
        unreachable!()
    };
    let addr = inner.local_addr().unwrap();

    let server_task = compio::runtime::spawn(async move {
        let mut conn = listener.accept().await.expect("accept failed");
        authenticate_server(&mut conn, &hmac()).await
    });

    let mut conn = Conn::connect_tcp(addr).await.expect("connect failed");
    let client_result = authenticate_client(&mut conn, &HandshakeMode::hmac(b"wrong".to_vec())).await;
    let client_err = client_result.expect_err("client must be rejected");
    assert!(matches!(
        client_err.current_context(),
        HandshakeError::Rejected
    ));

    let server_err = server_task
        .await
        .unwrap()
        .expect_err("server must reject");
    assert!(matches!(
        server_err.current_context(),
        HandshakeError::InvalidProof
    ));
}

/// `ConnectionMode::OneToOne`: the second concurrent client is turned away
/// with an explicit rejection before any handshake bytes are exchanged.
#[compio::test]
async fn one_to_one_rejects_second_client() {
    let endpoint = Endpoint::Tcp("127.0.0.1:0".parse().unwrap());
    let listener = endpoint.listen().await.expect("listen failed");
    let Listener::Tcp(inner) = &listener else {
        unreachable!()
    };
    let addr = inner.local_addr().unwrap();

    let server_task = compio::runtime::spawn(async move {
        let gate = ConnectionGate::new(ConnectionMode::OneToOne);
        // First client: acquires the lease, handshakes, session lives until
        // the client drops.
        let mut first = listener.accept().await.expect("accept first failed");
        let _lease = gate.try_acquire().expect("first client must acquire the gate");
        authenticate_server(&mut first, &hmac())
            .await
            .expect("first handshake failed");
        let first_session = spawn_session::<echo::Client, _>(
            first,
            Side::Server,
            EchoImpl,
        );

        // Second client: gate is taken, reject before the handshake.
        let mut second = listener.accept().await.expect("accept second failed");
        assert!(gate.try_acquire().is_none());
        reject_connection(&mut second, "one-to-one server is busy")
            .await
            .expect("reject failed");

        compio::time::timeout(std::time::Duration::from_secs(5), first_session.wait())
            .await
            .ok();
    });

    let first_session = connect_client(Conn::connect_tcp(addr).await.expect("connect failed")).await;
    assert_eq!(ping(&first_session, "first").await, "echo first");

    let mut second_conn = Conn::connect_tcp(addr).await.expect("connect failed");
    let second_result = authenticate_client(&mut second_conn, &hmac()).await;
    let err = second_result.expect_err("second client must be rejected");
    assert!(matches!(
        err.current_context(),
        HandshakeError::Rejected
    ));

    drop(first_session);
    server_task.await.unwrap();
}

/// A message beyond the traversal limit must not be processed: the
/// handler never runs and the server tears the connection down. The session
/// here uses a deliberately tiny limit so the test doesn't move megabytes.
#[compio::test]
async fn oversized_message_trips_traversal_limit() {
    use ogurpchik::rpc::spawn_session_with_options;

    let tiny_limits = || {
        let mut options = capnp::message::ReaderOptions::new();
        options.traversal_limit_in_words(Some(8 * 1024)); // 64 KiB
        options
    };
    let handler_ran = std::rc::Rc::new(std::cell::Cell::new(false));

    let endpoint = Endpoint::Tcp("127.0.0.1:0".parse().unwrap());
    let listener = endpoint.listen().await.expect("listen failed");
    let Listener::Tcp(inner) = &listener else {
        unreachable!()
    };
    let addr = inner.local_addr().unwrap();

    let server_task = compio::runtime::spawn({
        let handler_ran = handler_ran.clone();
        async move {
            struct TrackImpl(std::rc::Rc<std::cell::Cell<bool>>);
            impl echo::Server for TrackImpl {
                async fn ping(
                    self: Rc<Self>,
                    _params: echo::PingParams,
                    _results: echo::PingResults,
                ) -> Result<(), capnp::Error> {
                    self.0.set(true);
                    Ok(())
                }
            }

            let mut conn = listener.accept().await.expect("accept failed");
            authenticate_server(&mut conn, &hmac())
                .await
                .expect("server handshake failed");
            let session = spawn_session_with_options::<echo::Client, _>(
                conn,
                Side::Server,
                TrackImpl(handler_ran),
                tiny_limits(),
            );
            // If the limit is enforced, the read side errors and wait()
            // returns; if it is not, the ping would be answered and wait()
            // would keep hanging until this timeout fires.
            compio::time::timeout(std::time::Duration::from_secs(5), session.wait())
                .await
                .expect("server session hung: the oversized message got through")
                .ok();
        }
    });

    let mut conn = Conn::connect_tcp(addr).await.expect("connect failed");
    authenticate_client(&mut conn, &hmac())
        .await
        .expect("client handshake failed");
    let client_session = spawn_session_with_options::<echo::Client, _>(
        conn,
        Side::Client,
        EchoImpl,
        tiny_limits(),
    );

    let big = "x".repeat(1024 * 1024); // 1 MiB >> 64 KiB traversal limit
    let mut req = client_session.remote().ping_request();
    req.get().set_msg(&big[..]);
    let _ = req.send();

    server_task.await.unwrap();
    assert!(
        !handler_ran.get(),
        "handler ran: the traversal limit was not enforced"
    );
    drop(client_session);
}
