# oguRPChik 🥒

<div align="center">
  <img src="/Unusual-Bananas-0.png" width="350" alt="Ogurpchik Logo">
  <br>

[![Crates.io](https://img.shields.io/crates/v/ogurpchik.svg)](https://crates.io/crates/ogurpchik)
[![Docs.rs](https://docs.rs/ogurpchik/badge.svg)](https://docs.rs/ogurpchik)
[![License](https://img.shields.io/crates/l/ogurpchik.svg)](LICENSE)
</div>

> **A transport-agnostic RPC framework for stream and memory-based communication. Built with high-performance primitives to deliver medium-performance results.**

## 🧐 Motivation

This crate is actively used in my main project for duplex communication (`Host <-> VM`, `Host <-> Plugins`).

However, let's be honest: I did not extract it into a separate library for "better modularity" or "architectural purity". I did it because the pun **Ogurpchik** (`Ogurets` + `RPC`) popped into my head, and I needed a public repository to make the joke official.

## 🚀 WHAT THIS PICKLE CAN DO (for very smol ones)

- **Transport Agnostic**: Works over TCP, VSOCK, SHM, named pipes, or any other backend you choose to implement.
- **Message Flexible**: Supports both data-owning (`serde`-style) and zero-copy view formats (`rkyv`).
- **Service Discovery**: Windows registry-backed discovery with no broker process and no extra service.
- **Handshake Modes**: Disabled by default, with optional version-only, HMAC, and Windows-only signed-process auth.
- **Connection Policy**: Servers can run as `one-to-one` or `one-to-many`.

## 📦 Transports

| Transport | Description |
|-----------|-------------|
| `TcpTransport` | TCP sockets |
| `VsockTransport` | Hyper-V / Linux VM sockets |
| `ShmTransport` | Shared memory IPC |
| `NamedPipeTransport` | Windows named pipes |

## 📖 Usage

### Define your protocol

```rust
use ogurpchik::codecs::rkyv_protocol::RkyvCodec;
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize)]
pub enum Request {
    Ping,
    Echo(String),
}

#[derive(Archive, Serialize, Deserialize)]
pub enum Response {
    Pong,
    Echo(String),
}

pub type MyCodec = RkyvCodec<Request, Response>;
```

### Implement a handler

```rust
use ogurpchik::high::service_handler::ServiceHandler;

#[derive(Clone)]
struct MyHandler;

impl ServiceHandler<MyCodec> for MyHandler {
    async fn on_request<'a>(&self, req: &ArchivedRequest) -> anyhow::Result<Response> {
        match req {
            ArchivedRequest::Ping => Ok(Response::Pong),
            ArchivedRequest::Echo(s) => Ok(Response::Echo(s.to_string())),
        }
    }
}
```

### Single process (loopback)

```rust
use ogurpchik::high::node::Node;
use ogurpchik::transport::stream::adapters::tcp::TcpTransport;

#[compio::main]
async fn main() -> anyhow::Result<()> {
    let (client, _guard) = Node::new()?
        .serve::<MyCodec, _, _>(TcpTransport::new("127.0.0.1:1337"), MyHandler)
        .connect::<MyCodec, _>(TcpTransport::new("127.0.0.1:1337"))
        .start()
        .await?;

    let response = client.call(Request::Ping).await?;
    Ok(())
}
```

### Handshake and connection policy

`Node` starts with handshake disabled and `one-to-many` server behavior.

```rust
use ogurpchik::high::node::Node;

let node = Node::new()?
    .auth_hmac("dev-secret")
    .one_to_one();
```

Available helpers:

- `auth_hmac(secret)` - transport-agnostic shared-secret authentication
- `handshake_version_only()` - protocol/version bootstrap without auth
- `auth_signed_process(public_key)` - Windows-only PID plus detached-signature verification
- `auth_disabled()` - explicitly disable the handshake
- `one_to_one()` / `one_to_many()` - server-side connection policy

`auth_signed_process(public_key)` expects the connecting process image to have a sibling detached signature file such as `gui.exe.sig`. The server validates the image bytes with the provided Ed25519 public key. The key may be raw 32-byte material or base64 text.

### Host <-> VM (Hyper-V / WSL2)

The host registers its VM address in the Windows registry on startup. Both sides discover each other without any prior coordination.

**Host side (Windows):**

```rust
use ogurpchik::discovery::register_vm_default;
use ogurpchik::high::node::Node;
use ogurpchik::transport::stream::adapters::vsock::{VsockAddr, VsockTransport};

#[compio::main]
async fn main() -> anyhow::Result<()> {
    register_vm_default("WSL")?;

    let (guest_client, _guard) = Node::new()?
        .serve::<AgentCodec, _, _>(VsockTransport::server(VsockAddr::SelfManaged, 5000), HostHandler {})
        .publish("HOST")
        .connect::<HostCodec, _>(VsockTransport::client(VsockAddr::SelfManaged))
        .wait_for("GUEST")
        .start()
        .await?;

    Ok(())
}
```

**Guest side (WSL2 / Linux VM):**

```rust
#[compio::main]
async fn main() -> anyhow::Result<()> {
    let (host_client, _guard) = Node::new()?
        .serve::<HostCodec, _, _>(VsockTransport::server(VsockAddr::SelfManaged, 5001), GuestHandler)
        .publish("GUEST")
        .connect::<AgentCodec, _>(VsockTransport::client(2))
        .wait_for("HOST")
        .start()
        .await?;

    Ok(())
}
```

Each side publishes itself to the Windows registry and waits for the other to appear. `_guard` holds the registry entry alive and dropping it cleans the registration up automatically.

### Serve only

```rust
#[compio::main]
async fn main() -> anyhow::Result<()> {
    let _guard = Node::new()?
        .serve::<MyCodec, _, _>(TcpTransport::new("127.0.0.1:1337"), MyHandler)
        .publish("MY_SERVICE")
        .start()
        .await?;

    Ok(())
}
```

### Connect only

```rust
#[compio::main]
async fn main() -> anyhow::Result<()> {
    let client = Node::new()?
        .connect::<MyCodec, _>(TcpTransport::new("127.0.0.1:1337"))
        .wait_for("MY_SERVICE")
        .start()
        .await?;

    Ok(())
}
```

## Service Discovery

Discovery is backed by the Windows registry (`HKCU\Software\Ogurpchik\Services`).

- **Publish**: when a server is ready, its topology is written to the registry under a service name
- **Resolve**: clients read the topology by name before connecting
- **Watch**: clients block until the key appears; native on Windows, polled via `reg.exe` on WSL/Linux
- **Cleanup**: `ServiceRegistration` deletes the registry key on drop

```text
HKCU\Software\Ogurpchik\
  Services\
    host  -> {"transport_kind":"vsock","codec_kind":"rkyv","map":{"0":"2:5000"}}
    guest -> {"transport_kind":"vsock","codec_kind":"rkyv","map":{"0":"550e8400-e29b-41d4-a716-446655440000:5001"}}
  Hosts\
    WSL   -> "550e8400-e29b-41d4-a716-446655440000"
```

## License

MIT
