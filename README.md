# oguRPChik 🥒
<div>
  <img src="/Unusual-Bananas-0.png" width="350" alt="Ogurpchik Logo">
  <br>

[![Crates.io](https://img.shields.io/crates/v/ogurpchik.svg)](https://crates.io/crates/ogurpchik)
[![Docs.rs](https://docs.rs/ogurpchik/badge.svg)](https://docs.rs/ogurpchik)
[![License](https://img.shields.io/crates/l/ogurpchik.svg)](LICENSE)
</div>

> **A transport-agnostic RPC framework for stream and memory-based communication. Built with high-performance primitives to deliver medium-performance results.**

## 🧐 Motivation

This crate is actively used in my main project for duplex communication (Host <-> VM, Host <-> Plugins).

However, let's be honest: I didn't extract it into a separate library for "better modularity" or "architectural purity". I did it because the pun **Ogurpchik** (*Ogurets* + *RPC*) popped into my head, and I simply needed a public repository to make the joke official.

## 🚀 Features

- **Transport Agnostic**: Works over TCP, VSOCK, SHM, or any other communication backend you choose to implement.
- **Message Flexible**: Supports both data-owning (Serde) and zero-copy view formats (rkyv).
- **Service Discovery**: Windows registry-backed discovery — no broker process, no handshakes, no magic.

## 📦 Transports

| Transport | Description |
|-----------|-------------|
| `TcpTransport` | TCP sockets, works everywhere |
| `VsockTransport` | Hyper-V / Linux VM sockets |
| `ShmTransport` | Shared memory IPC |

## 📖 Usage

### Define your protocol

```rust
use rkyv::{Archive, Serialize, Deserialize};
use ogurpchik::codecs::rkyv_protocol::RkyvCodec;

#[derive(Archive, Serialize, Deserialize)]
pub enum Request { Ping, Echo(String) }

#[derive(Archive, Serialize, Deserialize)]
pub enum Response { Pong, Echo(String) }

pub type MyCodec = RkyvCodec<Request, Response>;
```

### Implement a handler

```rust
use ogurpchik::service_handler::ServiceHandler;

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
use ogurpchik::node::Node;
use ogurpchik::transport::stream::adapters::tcp::TcpTransport;

#[compio::main]
async fn main() -> anyhow::Result<()> {
    let (client, _guard) = Node::new()
        .serve::<MyCodec, _, _>(TcpTransport::new("127.0.0.1"), MyHandler)
        .connect::<MyCodec, _>(TcpTransport::new("127.0.0.1"))
        .start()
        .await?;

    let response = client.call(Request::Ping).await?;
    Ok(())
}
```

### Host ↔ VM (Hyper-V / WSL2)

The host registers its VM address in the Windows registry on startup.
Both sides discover each other without any prior coordination.

**Host side (Windows):**

```rust
use ogurpchik::discovery::{register_vm_default, services};
use ogurpchik::node::Node;
use ogurpchik::transport::stream::adapters::vsock::VsockTransport;

#[compio::main]
async fn main() -> anyhow::Result<()> {
    // writes guest VMID to registry so the guest can find itself
    register_vm_default("WSL")?;

    let (guest_client, _guard) = Node::new()
        .serve::<AgentCodec, _, _>(VsockTransport::server(u32::MAX, 5000), HostHandler {})
        .publish(services::HOST)
        .connect::<HostCodec, _>(VsockTransport::client(u32::MAX))
        .wait_for(services::GUEST)
        .start()
        .await?;

    Ok(())
}
```

**Guest side (WSL2 / Linux VM):**

```rust
#[compio::main]
async fn main() -> anyhow::Result<()> {
    let (host_client, _guard) = Node::new()
        .serve::<HostCodec, _, _>(VsockTransport::server(u32::MAX, 5001), GuestHandler)
        .publish(services::GUEST)
        .connect::<AgentCodec, _>(VsockTransport::client(2))
        .wait_for(services::HOST)
        .start()
        .await?;

    Ok(())
}
```

Each side publishes itself to the Windows registry and waits for the other to appear.
`_guard` holds the registry entry alive — dropping it cleans up automatically.

### Serve only (no client)

```rust
#[compio::main]
async fn main() -> anyhow::Result<()> {
    let _guard = Node::new()
        .serve::<MyCodec, _, _>(VsockTransport::server(u32::MAX, 5000), MyHandler)
        .publish(services::MY_SERVICE)
        .start()
        .await?;

    Ok(())
}
```

### Connect only (no server)

```rust
#[compio::main]
async fn main() -> anyhow::Result<()> {
    let client = Node::new()
        .connect::<MyCodec, _>(VsockTransport::client(2))
        .wait_for(services::MY_SERVICE)
        .start()
        .await?;

    Ok(())
}
```

## 🔍 Service Discovery

Discovery is backed by the Windows registry (`HKCU\Software\Ogurpchik\Services`).

- **Publish**: when a server is ready, its topology is written to the registry under a service name.
- **Resolve**: clients read the topology by name before connecting.
- **Watch**: clients block until the key appears — works natively on Windows (`RegNotifyChangeKeyValue`), polled via `reg.exe` on WSL/Linux.
- **Cleanup**: `ServiceRegistration` guard deletes the registry key on drop.

```
HKCU\Software\Ogurpchik\
  Services\
    host    →  {"transport_kind":"vsock","codec_kind":"rkyv","map":{"0":"2:5000"}}
    guest   →  {"transport_kind":"vsock","codec_kind":"rkyv","map":{"0":"3:5001"}}
  Hosts\
    WSL     →  "550e8400-e29b-41d4-a716-446655440000"
```

## License

MIT