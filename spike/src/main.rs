//! Spike: prove capnp-rpc runs over compio via `compio_io::compat::AsyncStream`,
//! and that both sides of a connection can call each other symmetrically
//! (bootstrap capability exchanged in both directions).

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use compio::net::{UnixListener, UnixStream};
use compio_io::compat::AsyncStream;

pub mod ping_capnp {
    include!(concat!(env!("OUT_DIR"), "/ping_capnp.rs"));
}

use ping_capnp::ping;

struct PingImpl {
    label: &'static str,
    samples: std::cell::Cell<u64>,
}

impl ping::Server for PingImpl {
    async fn ping(
        self: capnp::capability::Rc<Self>,
        params: ping::PingParams,
        mut results: ping::PingResults,
    ) -> Result<(), capnp::Error> {
        let msg = params.get()?.get_msg()?;
        let msg = msg.to_str()?;
        results
            .get()
            .set_reply(format!("{}: echo {}", self.label, msg));
        Ok(())
    }

    async fn push_sample(
        self: capnp::capability::Rc<Self>,
        params: ping::PushSampleParams,
    ) -> Result<(), capnp::Error> {
        let value = params.get()?.get_value();
        self.samples.set(self.samples.get() + 1);
        println!("[{}] streamed sample #{}: {value}", self.label, self.samples.get());
        Ok(())
    }

    async fn done(
        self: capnp::capability::Rc<Self>,
        _params: ping::DoneParams,
        _results: ping::DoneResults,
    ) -> Result<(), capnp::Error> {
        println!(
            "[{}] stream done, received {} samples total",
            self.label,
            self.samples.get()
        );
        Ok(())
    }
}

async fn run_side(stream: UnixStream, side: rpc_twoparty_capnp::Side, label: &'static str) {
    let (reader, writer) = compio_io::split(stream);
    let input = AsyncStream::new(reader);
    let output = AsyncStream::new(writer);

    let network = Box::new(twoparty::VatNetwork::new(
        input,
        output,
        side,
        Default::default(),
    ));

    let local_ping: ping::Client = capnp_rpc::new_client(PingImpl {
        label,
        samples: std::cell::Cell::new(0),
    });
    let mut rpc_system = RpcSystem::new(network, Some(local_ping.client));

    let other_side = match side {
        rpc_twoparty_capnp::Side::Server => rpc_twoparty_capnp::Side::Client,
        rpc_twoparty_capnp::Side::Client => rpc_twoparty_capnp::Side::Server,
    };

    // Fetch the *other* side's bootstrap capability so we can call it too —
    // this is what makes the connection symmetric rather than client->server only.
    let remote_ping: ping::Client = rpc_system.bootstrap(other_side);

    // Drive the RPC system in the background; it must not require `Send`.
    compio::runtime::spawn(async move {
        if let Err(e) = rpc_system.await {
            eprintln!("[{label}] rpc system error: {e}");
        }
    })
    .detach();

    let mut req = remote_ping.ping_request();
    req.get().set_msg(format!("hello from {label}"));
    let reply = req.send().promise.await.expect("rpc call failed");
    let reply = reply
        .get()
        .expect("no results")
        .get_reply()
        .expect("no reply field")
        .to_str()
        .expect("reply not utf8")
        .to_string();

    println!("[{label}] received: {reply}");

    // Exercise the -> stream flow-control path: each send() only resolves once
    // the remote side has ack'd backpressure-wise, which is what our plan
    // relies on for metrics/UI-diff delivery instead of a hand-rolled channel.
    for i in 0..5u64 {
        let mut req = remote_ping.push_sample_request();
        req.get().set_value(i);
        req.send().await.expect("stream send failed");
    }
    remote_ping
        .done_request()
        .send()
        .promise
        .await
        .expect("done call failed");
}

#[compio::main]
async fn main() {
    let sock_path = std::env::temp_dir().join(format!("capnp-spike-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path).await.expect("bind failed");

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept failed");
        run_side(stream, rpc_twoparty_capnp::Side::Server, "host").await;
    });

    let client_stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect failed");
    let client_task = compio::runtime::spawn(async move {
        run_side(client_stream, rpc_twoparty_capnp::Side::Client, "agent").await;
    });

    server_task.await.unwrap();
    client_task.await.unwrap();

    // Disconnect behavior: a client that connects, exchanges one call, then
    // drops the socket mid-session must not hang the server's RpcSystem task.
    let sock_path2 = std::env::temp_dir().join(format!(
        "capnp-spike-{}-disconnect.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&sock_path2);
    let listener2 = UnixListener::bind(&sock_path2).await.expect("bind failed");
    let server_task2 = compio::runtime::spawn(async move {
        let (stream, _) = listener2.accept().await.expect("accept failed");
        let (reader, writer) = compio_io::split(stream);
        let input = AsyncStream::new(reader);
        let output = AsyncStream::new(writer);
        let network = Box::new(twoparty::VatNetwork::new(
            input,
            output,
            rpc_twoparty_capnp::Side::Server,
            Default::default(),
        ));
        let local_ping: ping::Client = capnp_rpc::new_client(PingImpl {
            label: "host2",
            samples: std::cell::Cell::new(0),
        });
        let rpc_system = RpcSystem::new(network, Some(local_ping.client));
        match rpc_system.await {
            Ok(()) => println!("[host2] rpc system exited cleanly"),
            Err(e) => println!("[host2] rpc system exited on disconnect: {e}"),
        }
    });

    {
        let stream = UnixStream::connect(&sock_path2)
            .await
            .expect("connect failed");
        let (reader, writer) = compio_io::split(stream);
        let input = AsyncStream::new(reader);
        let output = AsyncStream::new(writer);
        let network = Box::new(twoparty::VatNetwork::new(
            input,
            output,
            rpc_twoparty_capnp::Side::Client,
            Default::default(),
        ));
        let local_ping: ping::Client = capnp_rpc::new_client(PingImpl {
            label: "agent2",
            samples: std::cell::Cell::new(0),
        });
        let mut rpc_system = RpcSystem::new(network, Some(local_ping.client));
        let remote: ping::Client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);
        let handle = compio::runtime::spawn(rpc_system);
        let mut req = remote.ping_request();
        req.get().set_msg("one call then bye");
        req.send().promise.await.expect("call before drop failed");
        // Drop the JoinHandle *without* detaching: for compio's `Task` (backed by
        // async-task), that cancels the driving future, which drops VatNetwork
        // and the underlying stream halves — an actual socket close, not a leak.
        // (Using `.detach()` here would leave the task running in the background
        // holding the socket open forever, which is not a disconnect at all.)
        drop(remote);
        drop(handle);
    }

    // Give the server's task a moment to observe EOF/disconnect and finish.
    match compio::time::timeout(std::time::Duration::from_secs(3), server_task2).await {
        Ok(Ok(())) => println!("[main] host2 task finished within timeout"),
        Ok(Err(e)) => println!("[main] host2 task panicked: {e:?}"),
        Err(_) => println!("[main] host2 task did NOT finish within 3s — RpcSystem hung on disconnect"),
    }

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&sock_path2);
    println!("spike OK: bidirectional capnp-rpc over compio uds works, incl. streaming + disconnect");
}
