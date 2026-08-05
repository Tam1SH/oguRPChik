
pub mod conn;
pub mod listener;
#[cfg(windows)]
mod npipe;
pub mod vsock;

pub use conn::{Conn, PeerIdentity};
pub use listener::Listener;

pub trait Splitable {
    fn split(self) -> (Self, Self)
    where
        Self: Sized;
}
