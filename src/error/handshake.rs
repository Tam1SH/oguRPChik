use core::fmt;

/// Errors from the pre-RPC handshake (`auth::handshake`), which runs on the
/// raw connection before it is ever handed to `capnp_rpc::twoparty::VatNetwork`.
#[derive(Debug)]
#[non_exhaustive]
pub enum HandshakeError {
    /// Client and server disagree on the handshake wire version.
    UnsupportedVersion,
    /// Client and server were configured with different [`HandshakeMode`]
    /// schemes (e.g. one side expects HMAC, the other expects signed-process).
    ///
    /// [`HandshakeMode`]: crate::auth::handshake::HandshakeMode
    SchemeMismatch,
    /// The client's proof (HMAC, signed-process payload, ...) did not verify.
    InvalidProof,
    /// The server explicitly rejected the connection; the reason it gave is
    /// attached separately.
    Rejected,
    /// No response arrived within the handshake step timeout.
    Timeout,
    /// A handshake packet could not be decoded, or had a trailing/missing
    /// byte count.
    MalformedPacket,
    /// [`HandshakeMode::SignedProcess`] was configured on a transport that
    /// cannot attest the peer's process identity (i.e.
    /// `Conn::peer_identity()` returns `PeerIdentity::Unknown` for it). This
    /// is refused at server start, not silently downgraded.
    ///
    /// [`HandshakeMode::SignedProcess`]: crate::auth::handshake::HandshakeMode::SignedProcess
    PeerAttestationUnavailable,
    /// The connecting process's image signature did not verify against the
    /// configured public key, or its `PID` was reused by a different process
    /// between attestation and verification.
    SignedProcessVerificationFailed,
    /// The server is in `one_to_one` mode and already has an active session.
    ConnectionLimitReached,
    /// A local OS facility or the transport itself failed mid-handshake
    /// (the peer went away, the RNG failed, ...). The underlying
    /// `TransportError`/`io::Error` is attached or layered underneath.
    Io,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => f.write_str("unsupported handshake version"),
            Self::SchemeMismatch => f.write_str("handshake auth scheme mismatch"),
            Self::InvalidProof => f.write_str("invalid handshake proof"),
            Self::Rejected => f.write_str("handshake rejected by peer"),
            Self::Timeout => f.write_str("handshake timed out"),
            Self::MalformedPacket => f.write_str("malformed handshake packet"),
            Self::PeerAttestationUnavailable => f.write_str(
                "signed-process auth requires a transport that can attest the peer's identity",
            ),
            Self::SignedProcessVerificationFailed => {
                f.write_str("signed-process verification failed")
            }
            Self::ConnectionLimitReached => {
                f.write_str("connection rejected: server is in one-to-one mode")
            }
            Self::Io => f.write_str("i/o failure during handshake"),
        }
    }
}

impl core::error::Error for HandshakeError {}
