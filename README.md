# oguRPChik 🥒

<div align="center">
  <img src="/Unusual-Bananas-0.png" width="350" alt="Ogurpchik Logo">
  <br>

[![Crates.io](https://img.shields.io/crates/v/ogurpchik.svg)](https://crates.io/crates/ogurpchik)
[![Docs.rs](https://docs.rs/ogurpchik/badge.svg)](https://docs.rs/ogurpchik)
[![License](https://img.shields.io/crates/l/ogurpchik.svg)](LICENSE)
</div>

> **Host ↔ VM-agent and host ↔ plugin RPC: own compio transport (vsock/uds/npipe), capnp-rpc on top.**

## 🧐 Motivation

This crate is actively used in my main project for duplex communication (`Host <-> VM`, `Host <-> Plugins`).

However, let's be honest: I did not extract it into a separate library for "better modularity" or "architectural purity". I did it because the pun **Ogurpchik** (`Ogurets` + `RPC`) popped into my head, and I needed a public repository to make the joke official.

## 🚀 WHAT THIS PICKLE CAN DO (for very smol ones)

- **RPC by capnp-rpc**: schema-first contracts (`.capnp`), field *and* method evolution for free, symmetric bootstrap — both sides call each other over one connection, `-> stream` methods with automatic flow control.
- **Own transport where the ecosystem has none**: Hyper-V `AF_HYPERV` / Linux vsock, Unix domain sockets, Windows named pipes, TCP for dev — one runtime enum, no generics anywhere.
- **OS-level authentication**: the handshake takes the peer's PID from the OS (`GetNamedPipeClientProcessId`, `SO_PEERCRED`), never from the wire, and verifies the process image's detached ed25519 signature. HMAC mode for host↔agent.
- **Discovery by convention**: `\\.\pipe\<app>.<service>` on Windows, `$XDG_RUNTIME_DIR/<app>/<service>.sock` on Linux, a per-service vsock port — no registry, no broker.
- **Single-threaded by design**: everything is `!Send` and lives on one compio runtime; no atomics, no mutexes.
- **Layered errors**: `error-stack` reports per layer (endpoint → transport → handshake → rpc); outgoing capnp exceptions pass an allowlist and never leak local details.

## 📦 Layout

| Path | What |
|------|------|
| `src/net/` | `Conn`/`Listener` runtime enum over vsock/uds/npipe/tcp + `peer_identity()` |
| `src/auth/` | handshake on the raw `Conn` (exact reads, before capnp starts) + signed-process |
| `src/rpc.rs` | `Conn` → `AsyncStream` → `VatNetwork` → `RpcSystem` bridge |
| `src/endpoint.rs` | the discovery convention + connect-with-backoff readiness |
| `src/error/` | `error-stack` contexts per layer + capnp exception boundary |

The crate is **schema-agnostic**: your `.capnp` contracts are your
application's business — compile them in your own crate with `capnpc`
(exactly like `tests/testschema/` does here) and hand the generated
bootstrap capability to `spawn_session`.

## 🛠 Build requirements

The library itself needs only a Rust toolchain. Compiling **your** schemas
shells out to the external C++ `capnp` binary (no pure-Rust equivalent
exists) — install it (`winget install capnproto.capnproto`,
`apt install capnproto`, `brew install capnp`, ...) in your project's
build environment; this applies to plugin authors too. If `-> stream`
methods fail to compile, point `CAPNP_INCLUDE_DIR` at the directory
containing `capnp/stream.capnp`.

## 📖 Usage sketch

```rust
use ogurpchik::auth::handshake::{HandshakeMode, authenticate_client, authenticate_server};
use ogurpchik::endpoint::Endpoint;
use ogurpchik::rpc::{Side, spawn_session};
use my_app_schema::my_interface; // your crate, your .capnp

// Server side (agent in the VM):
let listener = Endpoint::vsock_to_host(AGENT_PORT).listen().await?;
let mut conn = listener.accept().await?;
authenticate_server(&mut conn, &HandshakeMode::hmac(b"shared-secret".to_vec())).await?;
let session = spawn_session::<my_interface::Client, _>(conn, Side::Server, MyImpl);

// Client side (host): same steps with authenticate_client + Side::Client,
// then `session.remote()` is the server's bootstrap capability.
let reply = session.remote().ping_request().send().promise.await?;
```

The handshake runs on the raw `Conn` with exact-size reads; the byte after
the final `Ack` already belongs to capnp. Dropping the `RpcSession` cancels
the RPC driver and closes the connection.

## License

MIT
