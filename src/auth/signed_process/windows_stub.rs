use crate::error::{HandshakeError, Result};
use error_stack::Report;

fn unsupported() -> Report<HandshakeError> {
    Report::new(HandshakeError::SignedProcessVerificationFailed)
        .attach("signed-process attestation is only supported on Windows")
}

pub(crate) fn process_creation_time(_: u32) -> Result<u64, HandshakeError> {
    Err(unsupported())
}

pub(crate) fn verify_process_image(_: u32, _: &[u8]) -> Result<(), HandshakeError> {
    Err(unsupported())
}
