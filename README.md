# oguRPChik 🥒

<div align="center">
  <img src="/Unusual-Bananas-0.png" width="350" alt="Ogurpchik Logo">
</div>

RPC between host, VM agents and plugins over vsock / uds / named pipes.
Used in [uniproc](https://github.com/ignat/uniproc).

- capnp-rpc for the protocol (your `.capnp` schema is the contract)
- compio transport: Hyper-V `AF_HYPERV` / Linux vsock, Unix sockets, Windows named pipes, TCP
- pre-RPC handshake on the raw connection: HMAC or signed-process auth with the peer PID taken from the OS (`GetNamedPipeClientProcessId`, `SO_PEERCRED`), never from the wire
- discovery by convention: `\\.\pipe\<app>.<service>`, `$XDG_RUNTIME_DIR/<app>/<service>.sock`, per-service vsock port
- single-threaded, `!Send`, `error-stack` errors

## Hello world

Your schema (compiled with `capnpc` in your own crate):

```capnp
interface Echo {
  ping @0 (msg :Text) -> (reply :Text);
}
```

Server:

```rust
use ogurpchik::auth::handshake::HandshakeMode;
use ogurpchik::endpoint::Endpoint;
use ogurpchik::rpc::accept_session;

let endpoint = Endpoint::for_service("myapp", "echo")?;
let listener = endpoint.listen().await?;
let session = accept_session::<echo_capnp::echo::Client, _>(
    &listener,
    &HandshakeMode::hmac(b"secret".to_vec()),
    EchoImpl,
).await?;
```

Client:

```rust
use ogurpchik::rpc::connect_session;

let session = connect_session::<echo_capnp::echo::Client, _>(
    &endpoint,
    &HandshakeMode::hmac(b"secret".to_vec()),
    EchoImpl,
).await?;

let mut req = session.remote().ping_request();
req.get().set_msg("hello");
let reply = req.send().promise.await?;
```

## License

MIT
