//! Service discovery by convention — no registry, no KV store, no
//! discovery daemon. Both sides derive the same address from the same
//! service name:
//!
//! - Windows: `\\.\pipe\<app>.<service>` (named pipe — the only Windows
//!   transport with peer credentials, hence the plugin default there);
//! - Linux: `$XDG_RUNTIME_DIR/<app>/<service>.sock`, falling back to
//!   `/tmp/<app>-<uid>/`;
//! - host↔agent: vsock with the port as a per-service constant
//!   ([`Endpoint::vsock_to_host`] / [`Endpoint::vsock_to_best_vm`]).
//!
//! Readiness is equally conventional: the service is ready when connecting
//! succeeds, so [`Endpoint::connect_ready`] just retries with backoff.

use crate::error::{EndpointError, Result, TransportError};
use crate::net::vsock::VsockTarget;
use crate::net::{Conn, Listener};
use error_stack::{Report, ResultExt};
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// A concrete address a [`Listener`] binds or a [`Conn`] connects to.
/// Chosen at the call site, matched at runtime — deliberately not a type
/// parameter (see `net/mod.rs` docs).
#[derive(Debug, Clone)]
pub enum Endpoint {
    Vsock { target: VsockTarget, port: u32 },
    Uds(PathBuf),
    /// Full pipe path (`\\.\pipe\...`). Constructible on any platform so
    /// configs stay portable, but binding/connecting fails off Windows.
    Npipe(String),
    Tcp(SocketAddr),
}

impl Endpoint {
    /// The conventional endpoint for a service of application `app`.
    ///
    /// `app` and `service` become path/pipe-name components, so they are
    /// restricted to `[A-Za-z0-9_-]` — anything else is rejected rather
    /// than sanitized, because a silent rename means the two sides derive
    /// *different* addresses and the failure shows up miles away.
    pub fn for_service(app: &str, service: &str) -> Result<Self, EndpointError> {
        validate_name(app)?;
        validate_name(service)?;

        #[cfg(windows)]
        {
            Ok(Self::Npipe(format!(r"\\.\pipe\{app}.{service}")))
        }
        #[cfg(not(windows))]
        {
            Ok(Self::Uds(runtime_dir(app)?.join(format!("{service}.sock"))))
        }
    }

    /// Guest → host over vsock: CID 2 is `VMADDR_CID_HOST` on Linux and
    /// maps to `HV_GUID_PARENT` on Windows, so one constant expresses
    /// "the host" on both.
    pub fn vsock_to_host(port: u32) -> Self {
        Self::Vsock {
            target: VsockTarget::Cid(2),
            port,
        }
    }

    /// Host → the "best" local VM (WSL if present, else the sole running
    /// VM) over vsock, resolved via `vmcompute.dll`. Replaces the old
    /// `Hosts\<name>` registry lookup.
    #[cfg(windows)]
    pub fn vsock_to_best_vm(port: u32) -> Result<Self, EndpointError> {
        let guid = crate::net::vsock::utils::get_best_vmid()
            .change_context(EndpointError::InvalidVsockTarget)?;
        Ok(Self::Vsock {
            target: VsockTarget::Guid(crate::net::vsock::utils::guid_to_uuid(guid)),
            port,
        })
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Vsock { .. } => "vsock",
            Self::Uds(_) => "uds",
            Self::Npipe(_) => "npipe",
            Self::Tcp(_) => "tcp",
        }
    }

    pub async fn listen(&self) -> Result<Listener, TransportError> {
        match self {
            Self::Tcp(addr) => Listener::bind_tcp(*addr).await,
            Self::Uds(path) => Listener::bind_uds(path).await,
            #[cfg(windows)]
            Self::Npipe(name) => Listener::bind_npipe(name).await,
            #[cfg(not(windows))]
            Self::Npipe(_) => Err(Report::new(TransportError::Bind)
                .attach("named pipes are only available on Windows")),
            Self::Vsock { target, port } => Listener::bind_vsock(*target, *port),
        }
    }

    pub async fn connect(&self) -> Result<Conn, TransportError> {
        match self {
            Self::Tcp(addr) => Conn::connect_tcp(*addr).await,
            Self::Uds(path) => Conn::connect_uds(path).await,
            #[cfg(windows)]
            Self::Npipe(name) => Conn::connect_npipe(name).await,
            #[cfg(not(windows))]
            Self::Npipe(_) => Err(Report::new(TransportError::Connect)
                .attach("named pipes are only available on Windows")),
            Self::Vsock { target, port } => Conn::connect_vsock(*target, *port).await,
        }
    }

    /// Connect, retrying with exponential backoff until `give_up_after`
    /// elapses. "The service accepts connections" *is* the readiness
    /// signal — there is no separate registration step to wait for.
    pub async fn connect_ready(&self, give_up_after: Duration) -> Result<Conn, TransportError> {
        let start = std::time::Instant::now();
        let mut delay = Duration::from_millis(50);
        loop {
            match self.connect().await {
                Ok(conn) => return Ok(conn),
                Err(report) => {
                    if start.elapsed() + delay >= give_up_after {
                        return Err(report
                            .attach(format!("endpoint {self}"))
                            .attach("gave up waiting for the service to become ready"));
                    }
                    compio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(1));
                }
            }
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vsock { target, port } => write!(f, "vsock {target:?}:{port}"),
            Self::Uds(path) => write!(f, "uds {}", path.display()),
            Self::Npipe(name) => write!(f, "npipe {name}"),
            Self::Tcp(addr) => write!(f, "tcp {addr}"),
        }
    }
}

fn validate_name(name: &str) -> Result<(), EndpointError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(Report::new(EndpointError::InvalidServiceName).attach(format!("name: {name:?}")))
    }
}

/// `$XDG_RUNTIME_DIR/<app>`, falling back to `/tmp/<app>-<uid>`. The
/// directory is created if missing — `bind()` on a socket in a nonexistent
/// directory fails with a confusing `NotFound` otherwise.
#[cfg(not(windows))]
fn runtime_dir(app: &str) -> Result<PathBuf, EndpointError> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            // SAFETY: getuid has no failure mode.
            std::env::temp_dir().join(format!("{app}-{}", unsafe { libc::getuid() }))
        });
    let dir = base.join(app);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(Report::new(EndpointError::NoRuntimeDirectory)
            .attach(format!("cannot create {}: {e}", dir.display())));
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_validation() {
        assert!(Endpoint::for_service("myapp", "metrics-v2").is_ok());
        for bad in ["", "a/b", "a\\b", "a b", "a.b", "a:b", &"x".repeat(65)] {
            let report = Endpoint::for_service("myapp", bad)
                .expect_err("invalid service name must be rejected");
            assert!(matches!(
                report.current_context(),
                EndpointError::InvalidServiceName
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_convention_is_named_pipe() {
        match Endpoint::for_service("myapp", "metrics").unwrap() {
            Endpoint::Npipe(name) => assert_eq!(name, r"\\.\pipe\myapp.metrics"),
            other => panic!("expected npipe, got {other}"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_convention_is_uds_in_runtime_dir() {
        match Endpoint::for_service("myapp", "metrics").unwrap() {
            Endpoint::Uds(path) => assert!(path.ends_with("myapp/metrics.sock")),
            other => panic!("expected uds, got {other}"),
        }
    }

    #[compio::test]
    async fn tcp_endpoint_listen_connect_roundtrip() {
        let placeholder = Endpoint::Tcp("127.0.0.1:0".parse().unwrap());
        let listener = placeholder.listen().await.expect("listen failed");
        let Listener::Tcp(inner) = &listener else {
            unreachable!()
        };
        let endpoint = Endpoint::Tcp(inner.local_addr().expect("local_addr failed"));

        let (server, client) = futures::try_join!(listener.accept(), endpoint.connect())
            .expect("join failed");
        assert_eq!(server.kind(), "tcp");
        assert_eq!(client.kind(), "tcp");
    }

    #[cfg(windows)]
    #[compio::test]
    async fn conventional_npipe_endpoint_roundtrip() {
        let endpoint =
            Endpoint::for_service("ogurpchik-test", &format!("ep-{}", std::process::id()))
                .expect("convention failed");
        let listener = endpoint.listen().await.expect("listen failed");
        let (server, client) = futures::try_join!(listener.accept(), endpoint.connect())
            .expect("join failed");
        assert_eq!(server.kind(), "npipe");
        assert_eq!(client.kind(), "npipe");
    }

    #[compio::test]
    async fn connect_ready_succeeds_once_service_appears() {
        // Nothing is listening yet; start the listener shortly after.
        let placeholder = Endpoint::Tcp("127.0.0.1:0".parse().unwrap());
        let early = placeholder.listen().await.expect("listen failed");
        let Listener::Tcp(inner) = &early else {
            unreachable!()
        };
        let endpoint = Endpoint::Tcp(inner.local_addr().expect("local_addr failed"));
        drop(early); // free the port; connect_ready must outlast the gap

        let listener_task = compio::runtime::spawn({
            let endpoint = endpoint.clone();
            async move {
                compio::time::sleep(Duration::from_millis(200)).await;
                endpoint.listen().await.expect("delayed listen failed")
            }
        });

        let mut conn = endpoint
            .connect_ready(Duration::from_secs(5))
            .await
            .expect("connect_ready gave up too early");
        drop(conn);
        let listener = listener_task.await.unwrap();
        // Accept the pending connection to prove the pair is real.
        let server = listener.accept().await.expect("accept failed");
        assert_eq!(server.kind(), "tcp");
    }
}
