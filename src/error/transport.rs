use core::fmt;

#[derive(Debug)]
#[non_exhaustive]
pub enum TransportError {
    Bind,
    Connect,
    Accept,
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
