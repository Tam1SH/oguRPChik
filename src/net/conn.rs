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
    ///   (server-side) pipe handle. The client side has no pipe-API way to
    ///   learn the server's PID and gets `Unknown`.
    /// - `uds` (Linux): `SO_PEERCRED`.
    /// - `uds` (Windows and non-Linux unix), `vsock`, `tcp`: no
    ///   peer-credential facility exists, so this is `Unknown`.
    ///
    /// A failed OS call also yields `Unknown` rather than an error: this is
    /// a best-effort probe, and the caller's decision ("can I even attempt
    /// signed-process auth on this connection?") is the same either way.
    /// See `crate::auth::handshake` for why this distinction — not the wire
    /// protocol — decides whether signed-process auth is attemptable.
    pub fn peer_identity(&self) -> PeerIdentity {
        match self {
            #[cfg(windows)]
            Self::Npipe(NamedPipeStream::Server(server)) => npipe_client_pid(server),
            #[cfg(target_os = "linux")]
            Self::Uds(stream) => linux_uds_peer_pid(stream),
            _ => PeerIdentity::Unknown,
        }
    }
}

/// Windows: PID of the client process on the other end of an accepted pipe.
#[cfg(windows)]
fn npipe_client_pid(server: &compio::fs::named_pipe::NamedPipeServer) -> PeerIdentity {
    use compio::driver::AsRawFd;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;

    let mut pid = 0u32;
    // SAFETY: the handle is borrowed from the live pipe server for the
    // duration of the call; `pid` is a valid out-pointer.
    match unsafe { GetNamedPipeClientProcessId(HANDLE(server.as_raw_fd() as _), &mut pid) } {
        Ok(()) => PeerIdentity::Pid { pid },
        Err(_) => PeerIdentity::Unknown,
    }
}

/// Linux: PID of the peer of a unix-domain socket via `SO_PEERCRED`.
#[cfg(target_os = "linux")]
fn linux_uds_peer_pid(stream: &UnixStream) -> PeerIdentity {
    use std::os::fd::AsFd;

    match rustix::net::sockopt::socket_peercred(stream.as_fd()) {
        Ok(cred) => PeerIdentity::Pid {
            pid: cred.pid.as_raw_pid() as u32,
        },
        Err(_) => PeerIdentity::Unknown,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Listener;

    /// Windows named pipe, server side: the OS must report the client
    /// process's PID. The client here is this very test process, so the
    /// reported PID must equal ours — this is also the property the
    /// signed-process handshake relies on (PID from the OS, not the wire).
    #[cfg(windows)]
    #[compio::test]
    async fn npipe_server_peer_identity_is_os_reported_client_pid() {
        let name = format!("ogurpchik-peerid-test-{}", std::process::id());

        let listener = Listener::bind_npipe(&name).await.expect("bind failed");
        let accept = listener.accept();
        let connect = Conn::connect_npipe(&name);
        let (server, client) = futures::try_join!(accept, connect).expect("join failed");

        match server.peer_identity() {
            PeerIdentity::Pid { pid } => assert_eq!(pid, std::process::id()),
            PeerIdentity::Unknown => panic!("npipe server must see the client PID"),
        }
        // The client end has no pipe-API way to learn the server's PID.
        assert!(matches!(client.peer_identity(), PeerIdentity::Unknown));
    }

    /// Linux unix-domain socket: `SO_PEERCRED` must report the peer's PID —
    /// again this process, since client and server are the same test.
    #[cfg(target_os = "linux")]
    #[compio::test]
    async fn uds_peer_identity_is_os_reported_peer_pid() {
        let path = std::env::temp_dir().join(format!("ogurpchik-peerid-test-{}.sock", std::process::id()));

        let listener = Listener::bind_uds(&path).await.expect("bind failed");
        let accept = listener.accept();
        let connect = Conn::connect_uds(&path);
        let (server, _client) = futures::try_join!(accept, connect).expect("join failed");

        match server.peer_identity() {
            PeerIdentity::Pid { pid } => assert_eq!(pid, std::process::id()),
            PeerIdentity::Unknown => panic!("uds on Linux must see the peer PID"),
        }
    }

    /// Transports without a peer-credential facility report `Unknown` —
    /// never a guess, never wire-derived data.
    #[compio::test]
    async fn tcp_peer_identity_is_unknown() {
        let listener = Listener::bind_tcp("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind failed");
        let Listener::Tcp(inner) = &listener else {
            unreachable!()
        };
        let addr = inner.local_addr().expect("local_addr failed");

        let accept = listener.accept();
        let connect = Conn::connect_tcp(addr);
        let (server, client) = futures::try_join!(accept, connect).expect("join failed");

        assert!(matches!(server.peer_identity(), PeerIdentity::Unknown));
        assert!(matches!(client.peer_identity(), PeerIdentity::Unknown));
    }

    /// uds on Windows has no peer-credential facility (Windows AF_UNIX does
    /// not expose peer credentials), hence `Unknown` — which is exactly why
    /// npipe stays the default plugin transport on Windows.
    #[cfg(windows)]
    #[compio::test]
    async fn uds_peer_identity_is_unknown_on_windows() {
        let path = std::env::temp_dir().join(format!("ogurpchik-peerid-test-{}.sock", std::process::id()));

        let listener = Listener::bind_uds(&path).await.expect("bind failed");
        let accept = listener.accept();
        let connect = Conn::connect_uds(&path);
        let (server, _client) = futures::try_join!(accept, connect).expect("join failed");

        assert!(matches!(server.peer_identity(), PeerIdentity::Unknown));
    }
}
