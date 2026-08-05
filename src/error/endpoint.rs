use core::fmt;

/// Errors from resolving an [`Endpoint`](crate::endpoint::Endpoint) via the
/// service-name convention (see `endpoint.rs`): turning a service name into a
/// concrete uds path / named-pipe name / vsock port.
#[derive(Debug)]
#[non_exhaustive]
pub enum EndpointError {
    /// The service name contains characters that are not safe to embed in a
    /// filesystem path or a named-pipe name.
    InvalidServiceName,
    /// A vsock target (CID, GUID, or "self") could not be parsed or resolved.
    InvalidVsockTarget,
    /// The requested transport kind is not available on the current platform
    /// (e.g. named pipes on Linux, or signed-process auth over a transport
    /// that provides no peer credentials).
    UnsupportedOnPlatform,
    /// No runtime directory was available to place a uds socket in (neither
    /// `XDG_RUNTIME_DIR` nor a usable temp dir fallback).
    NoRuntimeDirectory,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServiceName => f.write_str("invalid service name"),
            Self::InvalidVsockTarget => f.write_str("invalid vsock target"),
            Self::UnsupportedOnPlatform => {
                f.write_str("transport not supported on this platform")
            }
            Self::NoRuntimeDirectory => f.write_str("no usable runtime directory"),
        }
    }
}

impl core::error::Error for EndpointError {}
