use crate::error::TransportError;
use compio::BufResult;
use compio::buf::{IoBuf, IoBufMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::{TcpStream, UnixStream};
use error_stack::{Report, ResultExt};
use std::net::SocketAddr;
use std::path::Path;

use crate::net::vsock::{VStream, VsockTarget};

#[cfg(windows)]
use crate::net::npipe::NamedPipeStream;

/// A connected, established transport, dispatched at runtime rather than
/// chosen by a type parameter — the whole point of this enum is that a
/// server or client picks its transport at the call site
/// (`Endpoint::Vsock{..}` vs `Endpoint::Uds(..)` etc.), not that generic code
/// gets written once per transport type.
///
/// `Clone` here is cheap: every variant wraps a handle that shares the
/// underlying OS resource on clone (compio's `TcpStream`/`UnixStream` do this
/// natively; `VStream`/`NamedPipeStream` do it the same way, deliberately, to
/// match). That's how full-duplex use works: `conn.clone()` for the read
/// side, `conn` for the write side, no locking needed.
#[derive(Clone)]
pub enum Conn {
    Tcp(TcpStream),
    Uds(UnixStream),
    #[cfg(windows)]
    Npipe(NamedPipeStream),
    Vsock(VStream),
}

/// What a transport can tell us about the identity of the process on the
/// other end, obtained strictly from the OS — never from anything the peer
/// sent over the wire. This is the first rung of the auth ladder (see
/// `crate::auth::signed_process`): a transport that returns `Unknown` here
/// cannot host `HandshakeMode::SignedProcess`, and the handshake refuses to
/// start rather than silently downgrading.
#[derive(Debug, Clone, Copy)]
pub enum PeerIdentity {
    Pid { pid: u32 },
    Unknown,
}

impl Conn {
    pub async fn connect_tcp(addr: SocketAddr) -> Result<Self, Report<TransportError>> {
        TcpStream::connect(addr)
            .await
            .map(Self::Tcp)
            .change_context(TransportError::Connect)
            .attach(format!("tcp {addr}"))
    }

    pub async fn connect_uds(path: &Path) -> Result<Self, Report<TransportError>> {
        UnixStream::connect(path)
            .await
            .map(Self::Uds)
            .change_context(TransportError::Connect)
            .attach(format!("uds {}", path.display()))
    }

    #[cfg(windows)]
    pub async fn connect_npipe(name: &str) -> Result<Self, Report<TransportError>> {
        crate::net::npipe::connect(name)
            .await
            .map(Self::Npipe)
            .change_context(TransportError::Connect)
            .attach(format!("npipe {name}"))
    }

    pub async fn connect_vsock(
        target: VsockTarget,
        port: u32,
    ) -> Result<Self, Report<TransportError>> {
        VStream::connect(target, port)
            .await
            .map(Self::Vsock)
            .change_context(TransportError::Connect)
            .attach(format!("vsock port {port}"))
    }

    /// Connects to a vsock listener bound with
    /// [`Listener::bind_vsock_loopback`], for same-host dev/test use when
    /// not actually inside a VM.
    ///
    /// This is *not* the same as `connect_vsock(VsockTarget::Cid(0), port)`:
    /// on Windows, `VStream`'s bind-side and connect-side CID→GUID mappings
    /// are asymmetric (bind treats any numeric CID as "listen for children",
    /// connect treats CID 0/1 as "loopback"), so picking "the same CID for
    /// both sides" is not the correct way to pair them up. Go through
    /// `VStream::connect_loopback` (which already encodes the right pairing)
    /// rather than re-deriving it here.
    pub async fn connect_vsock_loopback(port: u32) -> Result<Self, Report<TransportError>> {
        VStream::connect_loopback(port)
            .await
            .map(Self::Vsock)
            .change_context(TransportError::Connect)
            .attach(format!("vsock loopback port {port}"))
    }

    /// The kind name used in logs/errors — matches the `Endpoint` variant
    /// name, lowercase.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Tcp(_) => "tcp",
            Self::Uds(_) => "uds",
            #[cfg(windows)]
            Self::Npipe(_) => "npipe",
            Self::Vsock(_) => "vsock",
        }
    }

    /// Best-effort identity of the connected peer process, taken strictly
    /// from OS-level facilities:
    ///
    /// - `npipe` (Windows): `GetNamedPipeClientProcessId` on the accepted
    ///   pipe handle.
    /// - `uds` (Linux): `SO_PEERCRED`.
    /// - `uds` (Windows), `vsock`, `tcp`: no peer-credential facility exists,
    ///   so this is `Unknown`.
    ///
    /// See `crate::auth::handshake` for why this distinction — not the wire
    /// protocol — decides whether signed-process auth is even attemptable on
    /// a given connection. (Implemented in stage 3.)
    pub fn peer_identity(&self) -> PeerIdentity {
        PeerIdentity::Unknown
    }
}

impl AsyncRead for Conn {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            Self::Tcp(s) => s.read(buf).await,
            Self::Uds(s) => s.read(buf).await,
            #[cfg(windows)]
            Self::Npipe(s) => s.read(buf).await,
            Self::Vsock(s) => s.read(buf).await,
        }
    }
}

impl AsyncWrite for Conn {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        match self {
            Self::Tcp(s) => s.write(buf).await,
            Self::Uds(s) => s.write(buf).await,
            #[cfg(windows)]
            Self::Npipe(s) => s.write(buf).await,
            Self::Vsock(s) => s.write(buf).await,
        }
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush().await,
            Self::Uds(s) => s.flush().await,
            #[cfg(windows)]
            Self::Npipe(s) => s.flush().await,
            Self::Vsock(s) => s.flush().await,
        }
    }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(s) => s.shutdown().await,
            Self::Uds(s) => s.shutdown().await,
            #[cfg(windows)]
            Self::Npipe(s) => s.shutdown().await,
            Self::Vsock(s) => s.shutdown().await,
        }
    }
}
