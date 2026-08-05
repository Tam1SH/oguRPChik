//! Bridge from an authenticated [`Conn`] to a running capnp-rpc session.
//!
//! The full path, per connection:
//!
//! ```text
//! Conn (post-handshake)
//!   → conn.clone() twice (read half / write half — Clone shares the handle)
//!   → compio::io::compat::AsyncStream (completion→poll bridge)
//!   → capnp_rpc::twoparty::VatNetwork
//!   → capnp_rpc::RpcSystem
//!   → compio::runtime::spawn (local, !Send throughout — matches capnp-rpc's
//!     !Send design and this crate's single-threaded model)
//! ```
//!
//! Nothing here is buffered between the handshake and `VatNetwork`: the
//! handshake ([`crate::auth::handshake`]) reads exact sizes off the raw
//! `Conn`, so the first byte `VatNetwork` reads is the first byte of the
//! capnp session.

use crate::error::{RpcError, from_capnp_exception};
use crate::net::Conn;
use capnp::capability::{Client, FromClientHook, FromServer};
use capnp::message::ReaderOptions;
use capnp_rpc::{RpcSystem, twoparty};
use compio::io::compat::AsyncStream;
use compio::runtime::JoinHandle;
use error_stack::Report;

/// Which end of a twoparty connection this side is. Re-exported so callers
/// don't need to depend on `capnp-rpc` directly for the session API.
pub use capnp_rpc::rpc_twoparty_capnp::Side;

/// Reader limits, set explicitly because capnp's defaults
/// (8M words traversal = 64 MiB, 64 nesting) are sized for a *trusted*
/// peer. Plugins and VM agents are not trusted to that degree, so both
/// limits are tightened here. A peer exceeding them gets its connection
/// dropped, surfaced as [`RpcError::LimitExceeded`] — not unbounded memory
/// use.
pub fn default_reader_options() -> ReaderOptions {
    let mut options = ReaderOptions::new();
    options.traversal_limit_in_words(Some(1024 * 1024)); // 8 MiB
    options.nesting_limit(32);
    options
}

/// A live RPC session: the remote side's bootstrap capability plus the
/// handle driving the [`RpcSystem`].
///
/// **Shutdown semantics:** dropping the session drops the [`JoinHandle`]
/// *without* detaching it, and a dropped compio `JoinHandle` cancels the
/// task — the `RpcSystem` future is dropped, the `VatNetwork` with it, and
/// the underlying `Conn` actually closes. (`.detach()` would instead leave
/// the driver running in the background, holding the socket open forever —
/// this bit us in the spike's disconnect test.)
pub struct RpcSession<C: FromClientHook> {
    remote: C,
    driver: JoinHandle<Result<(), capnp::Error>>,
}

impl<C: FromClientHook> RpcSession<C> {
    /// The remote side's bootstrap capability — calls on it go over this
    /// connection. Cheap to clone (it's an `Rc` inside).
    pub fn remote(&self) -> &C {
        &self.remote
    }

    /// Waits for the RPC system to finish: a clean peer disconnect resolves
    /// `Ok(())` (`RpcSystem` itself downgrades `Disconnected` to `Ok`), any
    /// other failure becomes a [`Report`](error_stack::Report) via
    /// [`from_capnp_exception`].
    pub async fn wait(self) -> crate::error::Result<(), RpcError> {
        let Self { remote, driver } = self;
        drop(remote);
        match driver.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(from_capnp_exception(&e)),
            Err(panic) => Err(Report::new(RpcError::Setup)
                .attach(format!("rpc driver task panicked: {panic:?}"))),
        }
    }
}

/// Starts a symmetric twoparty RPC session over an already-authenticated
/// connection. `local_bootstrap` is this side's exported capability
/// implementation (a generated `Server` trait impl); the returned
/// [`RpcSession`] exposes the *remote* side's bootstrap capability, so both
/// ends can call each other over the single connection.
pub fn spawn_session<C, S>(conn: Conn, side: Side, local_bootstrap: S) -> RpcSession<C>
where
    C: FromServer<S> + FromClientHook,
    S: 'static,
{
    spawn_session_with_options(conn, side, local_bootstrap, default_reader_options())
}

pub fn spawn_session_with_options<C, S>(
    conn: Conn,
    side: Side,
    local_bootstrap: S,
    reader_options: ReaderOptions,
) -> RpcSession<C>
where
    C: FromServer<S> + FromClientHook,
    S: 'static,
{
    let local_client: C = capnp_rpc::new_client(local_bootstrap);
    let untyped = Client {
        hook: local_client.into_client_hook(),
    };

    let reader = AsyncStream::new(conn.clone());
    let writer = AsyncStream::new(conn);
    let network = Box::new(twoparty::VatNetwork::new(
        reader,
        writer,
        side,
        reader_options,
    ));

    let mut rpc_system = RpcSystem::new(network, Some(untyped));
    let remote_side = match side {
        Side::Server => Side::Client,
        Side::Client => Side::Server,
    };
    let remote: C = rpc_system.bootstrap(remote_side);

    RpcSession {
        remote,
        driver: compio::runtime::spawn(rpc_system),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::handshake::{HandshakeMode, authenticate_client, authenticate_server};
    use crate::net::Listener;
    use capnp::capability::Rc;
    use std::cell::RefCell;
    use std::time::Duration;
    use testschema::echo_capnp::echo;

    struct EchoImpl {
        label: &'static str,
        pings: RefCell<Vec<String>>,
    }

    impl echo::Server for EchoImpl {
        async fn ping(
            self: Rc<Self>,
            params: echo::PingParams,
            mut results: echo::PingResults,
        ) -> std::result::Result<(), capnp::Error> {
            let msg = params.get()?.get_msg()?;
            let msg = msg.to_str()?;
            self.pings.borrow_mut().push(msg.to_string());
            results.get().set_reply(format!("{}: echo {}", self.label, msg));
            Ok(())
        }
        // stat deliberately left at its default (unimplemented) — the
        // unimplemented test relies on that.
    }

    async fn uds_pair(tag: &str) -> (Conn, Conn) {
        let path = std::env::temp_dir().join(format!(
            "ogurpchik-rpc-test-{tag}-{}.sock",
            std::process::id()
        ));
        let listener = Listener::bind_uds(&path).await.expect("bind failed");
        let (server, client) =
            futures::try_join!(listener.accept(), Conn::connect_uds(&path)).expect("join failed");
        drop(listener);
        (server, client)
    }

    fn spawn_agent(conn: Conn, side: Side, label: &'static str) -> RpcSession<echo::Client> {
        spawn_session(
            conn,
            side,
            EchoImpl {
                label,
                pings: RefCell::new(Vec::new()),
            },
        )
    }

    async fn ping(remote: &echo::Client, msg: &str) -> String {
        let mut req = remote.ping_request();
        req.get().set_msg(msg);
        let reply = req.send().promise.await.expect("rpc call failed");
        reply
            .get()
            .expect("no results")
            .get_reply()
            .expect("no reply field")
            .to_str()
            .expect("reply not utf8")
            .to_string()
    }

    /// The property the exact-read handshake exists for: after the final
    /// Ack, the very next byte must already belong to capnp. If the
    /// handshake layer had buffered anything, this test would hang or
    /// corrupt.
    #[compio::test]
    async fn handshake_then_rpc_roundtrip() {
        let (mut server_conn, mut client_conn) = uds_pair("hs").await;
        let mode = || HandshakeMode::hmac(b"secret".to_vec());

        let server_task = compio::runtime::spawn(async move {
            authenticate_server(&mut server_conn, &mode())
                .await
                .expect("server handshake failed");
            let session = spawn_agent(server_conn, Side::Server, "host");
            // Answer one ping, then wait for the client to disconnect.
            compio::time::timeout(Duration::from_secs(5), session.wait())
                .await
                .expect("server session timed out")
                .expect("server session failed");
        });

        authenticate_client(&mut client_conn, &mode())
            .await
            .expect("client handshake failed");
        let session = spawn_agent(client_conn, Side::Client, "agent");
        let reply = ping(session.remote(), "hello").await;
        assert_eq!(reply, "host: echo hello");
        drop(session); // cancels the client driver, closing the connection

        server_task.await.unwrap();
    }

    /// Symmetric bootstrap: both sides export `Echo` and call each
    /// other over the same connection, simultaneously.
    #[compio::test]
    async fn bidirectional_calls_over_one_connection() {
        let (server_conn, client_conn) = uds_pair("bidi").await;
        let server_session = spawn_agent(server_conn, Side::Server, "host");
        let client_session = spawn_agent(client_conn, Side::Client, "agent");

        let (from_host, from_agent) = futures::join!(
            ping(client_session.remote(), "down"),
            ping(server_session.remote(), "up"),
        );
        assert_eq!(from_host, "host: echo down");
        assert_eq!(from_agent, "agent: echo up");
    }

    /// A method the server left at its default must come back as
    /// `unimplemented`, not a hang or a decode error — this is what schema
    /// evolution (new method ordinals) relies on for mixed-version peers.
    #[compio::test]
    async fn unimplemented_method_reports_unimplemented() {
        let (server_conn, client_conn) = uds_pair("unimpl").await;
        let _server_session = spawn_agent(server_conn, Side::Server, "host");
        let client_session = spawn_agent(client_conn, Side::Client, "agent");

        let req = client_session.remote().stat_request();
        let result = req.send().promise.await;
        let err = match result {
            Ok(_) => panic!("unimplemented method must fail"),
            Err(e) => e,
        };
        assert!(matches!(err.kind, capnp::ErrorKind::Unimplemented));
    }

    /// Dropping the client session (JoinHandle dropped, not detached) must
    /// close the connection and let the server's `wait()` resolve — no
    /// detached-driver socket leak.
    #[compio::test]
    async fn client_disconnect_lets_server_finish() {
        let (server_conn, client_conn) = uds_pair("drop").await;
        let server_session = spawn_agent(server_conn, Side::Server, "host");

        {
            let client_session = spawn_agent(client_conn, Side::Client, "agent");
            ping(client_session.remote(), "one call then bye").await;
            drop(client_session);
        }

        compio::time::timeout(Duration::from_secs(5), server_session.wait())
            .await
            .expect("server session did not finish within timeout")
            .expect("server session failed");
    }
}
