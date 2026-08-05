//! Transport layer: a runtime-selected [`Conn`]/[`Listener`] enum over
//! vsock, Unix domain sockets, Windows named pipes, and TCP (the last kept
//! mainly for local dev/test — the real targets are the first three).
//!
//! There is deliberately no generic `Transport<B>` trait here. The old code
//! had one, parametrized over buffer/allocator types to support a
//! zero-copy rkyv codec; that codec is gone (capnp-rpc owns framing and
//! buffers now), and with it the only reason for the generic layer to
//! exist. A plain enum, matched in a handful of places, is the whole
//! abstraction this crate needs.

pub mod conn;
pub mod listener;
#[cfg(windows)]
mod npipe;
pub mod vsock;

pub use conn::{Conn, PeerIdentity};
pub use listener::Listener;

/// A stream that can be split into an owned "read half" and an owned "write
/// half" that share the same underlying handle (clone-and-share, not a lock
/// or a channel) — the same shape `compio::net::{TcpStream, UnixStream}`
/// already have natively. Used by the per-transport unit tests to exercise
/// each stream type's duplex behavior directly; [`Conn`] itself doesn't need
/// this, since it's `Clone` and gets cloned wholesale instead (see
/// `conn.rs`).
pub trait Splitable {
    fn split(self) -> (Self, Self)
    where
        Self: Sized;
}
