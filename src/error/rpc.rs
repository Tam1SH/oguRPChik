use core::fmt;

use error_stack::Report;

/// Top-level, public error context. This is what a caller of `Client`/`Server`
/// sees; `report.downcast_ref::<TransportError>()` or
/// `report.downcast_ref::<HandshakeError>()` recover the layer below when
/// that level of detail is needed.
#[derive(Debug)]
#[non_exhaustive]
pub enum RpcError {
    /// Failed while establishing a connection. The transport/handshake
    /// layers underneath carry the specifics.
    Setup,
    /// The remote peer's capnp RPC layer returned an exception for a call
    /// this side made.
    Remote {
        /// The general nature of the remote failure — see
        /// [`capnp::ErrorKind`] for what each variant means operationally.
        kind: RemoteErrorKind,
    },
    /// This side's handler for an incoming call returned an error.
    Handler,
    /// A remote call targeted a method this side does not implement.
    Unimplemented,
    /// The peer's message exceeded a configured limit
    /// (`traversal_limit_in_words` / `nesting_limit`); the connection was
    /// dropped rather than risk unbounded memory use.
    LimitExceeded,
}

/// Mirrors the RPC-relevant subset of `capnp::ErrorKind`. `capnp::ErrorKind`
/// is `#[non_exhaustive]` and has many internal decode-error variants beyond
/// these four; anything else collapses to [`Other`](Self::Other) here rather
/// than growing this enum every time capnp adds a new internal kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteErrorKind {
    Failed,
    Overloaded,
    Disconnected,
    Unimplemented,
    Other,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup => f.write_str("failed to establish rpc connection"),
            Self::Remote { kind } => write!(f, "remote rpc error ({kind:?})"),
            Self::Handler => f.write_str("local handler failed"),
            Self::Unimplemented => f.write_str("method not implemented"),
            Self::LimitExceeded => f.write_str("message exceeded configured limits"),
        }
    }
}

impl core::error::Error for RpcError {}

/// The text of a remote exception, attached to a [`Report`] for local
/// diagnostics only.
///
/// **Never forward this to another peer.** By default a `capnp::Error`'s
/// `extra` text can contain anything the remote side's error path put there —
/// paths, PIDs, internal state — and in our topology the "remote side" can be
/// an untrusted plugin. See [`to_capnp_exception`] for the outgoing half of
/// this boundary.
#[derive(Debug)]
pub struct RemoteMessage(pub String);

impl fmt::Display for RemoteMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Converts an incoming `capnp::Error` (a remote exception we received) into
/// a `Report<RpcError>`. The remote's message text is preserved as a
/// [`RemoteMessage`] attachment for local logs; it is not part of the
/// `RpcError` context itself, so `current_context()` still matches cleanly on
/// `RpcError::Remote { kind }` without needing to inspect the text.
pub fn from_capnp_exception(err: &capnp::Error) -> Report<RpcError> {
    let kind = match err.kind {
        capnp::ErrorKind::Failed => RemoteErrorKind::Failed,
        capnp::ErrorKind::Overloaded => RemoteErrorKind::Overloaded,
        capnp::ErrorKind::Disconnected => RemoteErrorKind::Disconnected,
        capnp::ErrorKind::Unimplemented => RemoteErrorKind::Unimplemented,
        _ => RemoteErrorKind::Other,
    };
    Report::new(RpcError::Remote { kind }).attach(RemoteMessage(err.extra.clone()))
}

/// Converts a local `Report<C>` into the `capnp::Error` that gets sent to the
/// peer as an exception — **through an explicit allowlist**, not by
/// stringifying the whole report.
///
/// This is a security boundary, not a style choice: `Report`'s attachments
/// routinely carry local paths, PIDs, and other diagnostic detail (see
/// [`crate::auth`]), and the peer receiving this exception may be an
/// untrusted plugin. Only `context.to_string()` — the short, deliberately
/// written `Display` impl of the context enum itself — crosses the wire.
/// Everything attached to the report (the "why", as opposed to the "what")
/// stays in the local log.
///
/// `RpcError::Remote`/`RpcError::Setup` map to `Disconnected`/`Failed`
/// respectively so a peer that forwards an error it received from a third
/// party doesn't misreport it as its own failure.
pub fn to_capnp_exception<C>(report: &Report<C>) -> capnp::Error
where
    C: fmt::Display + Send + Sync + 'static,
{
    let description = report.current_context().to_string();
    if report.contains::<RpcError>() {
        match report.downcast_ref::<RpcError>() {
            Some(RpcError::Unimplemented) => capnp::Error::unimplemented(description),
            Some(RpcError::Remote { .. }) => capnp::Error::disconnected(description),
            _ => capnp::Error::failed(description),
        }
    } else {
        capnp::Error::failed(description)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TransportError;

    #[derive(Debug)]
    struct SensitivePath(std::path::PathBuf);

    impl fmt::Display for SensitivePath {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "socket path: {}", self.0.display())
        }
    }

    impl core::error::Error for SensitivePath {}

    #[test]
    fn layer_matching_survives_change_context() {
        let report = Report::new(TransportError::Connect)
            .attach("some local detail")
            .change_context(RpcError::Setup);

        assert!(matches!(report.current_context(), RpcError::Setup));
        assert!(report.contains::<TransportError>());
        assert!(matches!(
            report.downcast_ref::<TransportError>(),
            Some(TransportError::Connect)
        ));
    }

    #[test]
    fn outgoing_exception_never_carries_attachments() {
        // A handler error that accidentally attaches something sensitive
        // (a filesystem path, in this case) must not leak that text to the
        // peer — only the context's own Display output crosses the wire.
        let report = Report::new(SensitivePath(std::path::PathBuf::from(
            "/run/user/1000/very-secret-service.sock",
        )))
        .change_context(RpcError::Handler);

        let exception = to_capnp_exception(&report);

        assert_eq!(exception.kind, capnp::ErrorKind::Failed);
        assert!(!exception.extra.contains("secret"));
        assert!(!exception.extra.contains("/run/user"));
        assert_eq!(exception.extra, RpcError::Handler.to_string());
    }

    #[test]
    fn unimplemented_maps_to_capnp_unimplemented() {
        let report = Report::new(RpcError::Unimplemented);
        let exception = to_capnp_exception(&report);
        assert_eq!(exception.kind, capnp::ErrorKind::Unimplemented);
    }

    #[test]
    fn incoming_exception_roundtrip_preserves_kind_and_message() {
        let original = capnp::Error::disconnected("peer went away".to_string());
        let report = from_capnp_exception(&original);

        assert!(matches!(
            report.current_context(),
            RpcError::Remote {
                kind: RemoteErrorKind::Disconnected
            }
        ));
        let msg = report.downcast_ref::<RemoteMessage>().unwrap();
        assert_eq!(msg.0, "peer went away");
    }

    #[test]
    fn remote_forwarded_as_disconnected_not_local_failure() {
        // If we received a remote exception and, say, log-and-forward it as
        // our own outgoing exception, it must not be reported as if *we*
        // failed - the peer should see Disconnected/whatever RpcError::Remote
        // maps to, not a generic Failed that looks like our own bug.
        let incoming = from_capnp_exception(&capnp::Error::failed("upstream broke".to_string()));
        let outgoing = to_capnp_exception(&incoming);
        assert_eq!(outgoing.kind, capnp::ErrorKind::Disconnected);
    }
}
