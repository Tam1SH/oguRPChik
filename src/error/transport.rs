use core::fmt;

/// Errors from the transport layer (`net::conn`, `net::listener`): binding a
/// listener, connecting, accepting, or moving bytes on an established
/// connection.
///
/// The underlying `std::io::Error` is not stored in a variant field — attach
/// it instead (`report.attach(io_err)`), since `io::Error` already implements
/// `Display + Debug` and is a valid [`error_stack::Report`] attachment on its
/// own.
#[derive(Debug)]
#[non_exhaustive]
pub enum TransportError {
    /// Failed to bind a listener to an [`Endpoint`](crate::endpoint::Endpoint).
    Bind,
    /// Failed to connect to an [`Endpoint`](crate::endpoint::Endpoint).
    Connect,
    /// Failed to accept an incoming connection on an already-bound listener.
    Accept,
    /// An I/O error occurred on an established connection (read/write/split).
    Io,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind => f.write_str("failed to bind transport listener"),
            Self::Connect => f.write_str("failed to connect"),
            Self::Accept => f.write_str("failed to accept connection"),
            Self::Io => f.write_str("transport i/o error"),
        }
    }
}

impl core::error::Error for TransportError {}
