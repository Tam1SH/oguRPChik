#[cfg(all(feature = "npipe", windows))]
pub mod npipe;
#[cfg(feature = "shm")]
pub mod shm;
#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "uds")]
pub mod uds;
#[cfg(feature = "vsock")]
pub mod vsock;
