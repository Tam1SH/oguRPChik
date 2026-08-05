//! Hyper-V (`AF_HYPERV`, Windows) / virtio-vsock (`AF_VSOCK`, Linux) streams,
//! unified behind [`VStream`]/[`VListener`].
//!
//! Nothing in the wider ecosystem covers this: `tokio-vsock` is Linux-only,
//! and Microsoft's own `vmsocket` crate (which does unify both) is
//! synchronous and unpublished. This is one of the few places in the crate
//! that has to be genuinely our own code.

pub mod general;
#[cfg(unix)]
pub mod linux;
#[cfg(windows)]
pub mod utils;
#[cfg(windows)]
pub mod windows;

pub use general::{VListener, VStream};

use uuid::Uuid;

/// A logical vsock destination.
///
/// Linux (`AF_VSOCK`) addresses VMs by a numeric CID. Windows (`AF_HYPERV`)
/// addresses them by a GUID; [`utils::get_best_vmid`] resolves a
/// human-friendly VM name (e.g. `"WSL"`) to one.
#[derive(Debug, Clone, Copy)]
pub enum VsockTarget {
    Cid(u32),
    Guid(Uuid),
}
