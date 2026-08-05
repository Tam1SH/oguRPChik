
use crate::error::{HandshakeError, Result};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use error_stack::{Report, ResultExt};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
mod windows_stub;

#[cfg(windows)]
pub(crate) use windows::{process_creation_time, verify_process_image};
#[cfg(not(windows))]
pub(crate) use windows_stub::{process_creation_time, verify_process_image};

pub(crate) fn verify_signed_file(image_path: &Path, public_key: &[u8]) -> Result<(), HandshakeError> {
    let verifying_key = parse_verifying_key(public_key)?;
    let signature_path = signature_path_for(image_path);
    let image_bytes = fs::read(image_path)
        .change_context(HandshakeError::SignedProcessVerificationFailed)
        .attach(format!("failed to read signed image {}", image_path.display()))?;
    let signature = read_signature(&signature_path)?;
    verifying_key
        .verify(&image_bytes, &signature)
        .map_err(|e| {
            Report::new(HandshakeError::SignedProcessVerificationFailed)
                .attach(format!("signature verification failed: {e}"))
        })?;
    Ok(())
}

fn parse_verifying_key(public_key: &[u8]) -> Result<VerifyingKey, HandshakeError> {
    let decoded = if public_key.len() == 32 {
        public_key.to_vec()
    } else {
        let encoded = std::str::from_utf8(public_key).map_err(|_| {
            Report::new(HandshakeError::SignedProcessVerificationFailed)
                .attach("public key must be raw 32 bytes or base64 text")
        })?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .change_context(HandshakeError::SignedProcessVerificationFailed)
            .attach("failed to decode base64 public key")?
    };

    let key_bytes: [u8; 32] = decoded.try_into().map_err(|_| {
        Report::new(HandshakeError::SignedProcessVerificationFailed)
            .attach("ed25519 public key must be 32 bytes")
    })?;
    VerifyingKey::from_bytes(&key_bytes).change_context(HandshakeError::SignedProcessVerificationFailed)
}

fn read_signature(path: &Path) -> Result<Signature, HandshakeError> {
    let raw = match fs::read_to_string(path) {
        Ok(text) => match base64::engine::general_purpose::STANDARD.decode(text.trim()) {
            Ok(decoded) => decoded,
            Err(_) => fs::read(path)
                .change_context(HandshakeError::SignedProcessVerificationFailed)
                .attach(format!("failed to read signature {}", path.display()))?,
        },
        Err(_) => fs::read(path)
            .change_context(HandshakeError::SignedProcessVerificationFailed)
            .attach(format!("failed to read signature {}", path.display()))?,
    };
    Signature::from_slice(&raw).change_context(HandshakeError::SignedProcessVerificationFailed)
}

fn signature_path_for(image_path: &Path) -> PathBuf {
    let mut os = image_path.as_os_str().to_owned();
    os.push(".sig");
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::verify_signed_file;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        std::env::temp_dir().join(format!("ogurpchik-{name}-{unique}"))
    }

    #[test]
    fn verify_signed_file_accepts_detached_signature() {
        let image_path = temp_file_path("signed-process.exe");
        let sig_path = PathBuf::from(format!("{}.sig", image_path.display()));
        let image = b"fake gui binary";
        let secret_key = [7u8; 32];
        let signing_key = SigningKey::from_bytes(&secret_key);
        let signature = signing_key.sign(image);

        fs::write(&image_path, image).expect("write image");
        fs::write(
            &sig_path,
            base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        )
        .expect("write signature");

        verify_signed_file(&image_path, &signing_key.verifying_key().to_bytes())
            .expect("signature should verify");

        let _ = fs::remove_file(&image_path);
        let _ = fs::remove_file(&sig_path);
    }
}
