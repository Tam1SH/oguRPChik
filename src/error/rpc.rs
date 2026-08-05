use core::fmt;

use error_stack::Report;

#[derive(Debug)]
#[non_exhaustive]
pub enum RpcError {
    Setup,
    Remote {
        kind: RemoteErrorKind,
    },
    Handler,
    Unimplemented,
    LimitExceeded,
}

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

#[derive(Debug)]
pub struct RemoteMessage(pub String);

impl fmt::Display for RemoteMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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
        let incoming = from_capnp_exception(&capnp::Error::failed("upstream broke".to_string()));
        let outgoing = to_capnp_exception(&incoming);
        assert_eq!(outgoing.kind, capnp::ErrorKind::Disconnected);
    }
}
