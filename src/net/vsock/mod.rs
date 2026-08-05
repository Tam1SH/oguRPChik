
pub mod general;
#[cfg(unix)]
pub mod linux;
#[cfg(windows)]
pub mod utils;
#[cfg(windows)]
pub mod windows;

pub use general::{VListener, VStream};

use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum VsockTarget {
    Cid(u32),
    Guid(Uuid),
}
