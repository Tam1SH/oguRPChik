
#[cfg(windows)]
use crate::error::{HandshakeError, Result};
#[cfg(windows)]
use base64::Engine;
#[cfg(windows)]
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
#[cfg(windows)]
use error_stack::{Report, ResultExt};
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
mod windows_stub;

#[cfg(windows)]
pub(crate) use windows::{process_creation_time, verify_process_image};
#[cfg(not(windows))]
pub(crate) use windows_stub::{process_creation_time, verify_process_image};

#[cfg(windows)]
pub(crate) fn verify_signed_file(image_path: &Path, public_key: &[u8]) -> Result<(), HandshakeError> {
    let verifying_key = parse_verifying_key(public_key)?;
    let signature_path = signature_path_for(image_path);
    let image_bytes = fs::read(image_path)
        .change_context(HandshakeError::SignedProcessVerificationFailed)
        .attach(format!("failed to read signed image {}", image_path.display()))?;

    let Some(signature) = read_signature(&signature_path)? else {
        return on_missing_signature(image_path, &signature_path);
    };

    verifying_key
        .verify(&image_bytes, &signature)
        .map_err(|e| {
            Report::new(HandshakeError::SignedProcessVerificationFailed)
                .attach(format!("signature verification failed: {e}"))
        })?;
    Ok(())
}

/// An *absent* signature is tolerated in debug builds so a freshly rebuilt,
/// unsigned binary can still connect during development - re-signing after
/// every `cargo build` is not a workable inner loop.
///
/// Deliberately narrow: this covers only a missing file. A signature that is
/// present but does not verify stays fatal in every profile, so the relaxed
/// mode can never mask a real mismatch or a tampered image.
///
/// The switch is `debug_assertions` on *this crate*, which follows the profile
/// of the binary that links it: an agent built with `--release` always
/// enforces, and nothing at runtime can flip that.
#[cfg(all(windows, debug_assertions))]
fn on_missing_signature(image_path: &Path, signature_path: &Path) -> Result<(), HandshakeError> {
    tracing::warn!(
        image = %image_path.display(),
        signature = %signature_path.display(),
        "signed-process: no signature file; accepting the peer because this is a debug build - \
         a release build rejects it"
    );
    Ok(())
}

#[cfg(all(windows, not(debug_assertions)))]
fn on_missing_signature(image_path: &Path, signature_path: &Path) -> Result<(), HandshakeError> {
    Err(Report::new(HandshakeError::SignedProcessVerificationFailed)
        .attach(format!("no signature at {}", signature_path.display()))
        .attach(format!("for image {}", image_path.display())))
}

#[cfg(windows)]
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

/// `Ok(None)` means the file is simply not there - the one case
/// [`on_missing_signature`] is allowed to wave through. Every other read
/// failure stays an error, so an unreadable or corrupt signature is never
/// mistaken for an unsigned dev build.
#[cfg(windows)]
fn read_signature(path: &Path) -> Result<Option<Signature>, HandshakeError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Report::new(HandshakeError::SignedProcessVerificationFailed)
                .attach(format!("failed to read signature {}: {e}", path.display())));
        }
    };

    // Written either base64-encoded or as the raw 64 signature bytes.
    let raw = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text.trim()).ok())
        .unwrap_or(bytes);

    Signature::from_slice(&raw)
        .change_context(HandshakeError::SignedProcessVerificationFailed)
        .map(Some)
}

#[cfg(windows)]
fn signature_path_for(image_path: &Path) -> PathBuf {
    let mut os = image_path.as_os_str().to_owned();
    os.push(".sig");
    PathBuf::from(os)
}

#[cfg(all(test, windows))]
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

    /// The dev relaxation: an unsigned binary is let through in debug builds
    /// only. Same call must fail in release, which is what the `cfg` asserts.
    #[test]
    fn missing_signature_is_accepted_only_in_debug_builds() {
        let image_path = temp_file_path("unsigned-process.exe");
        fs::write(&image_path, b"freshly rebuilt, never signed").expect("write image");
        let public_key = SigningKey::from_bytes(&[7u8; 32]).verifying_key().to_bytes();

        let result = verify_signed_file(&image_path, &public_key);

        if cfg!(debug_assertions) {
            result.expect("a debug build must accept a missing signature");
        } else {
            result.expect_err("a release build must reject a missing signature");
        }

        let _ = fs::remove_file(&image_path);
    }

    /// The relaxation must not widen past "file absent": a signature that is
    /// present but wrong stays fatal even in a debug build, or dev mode would
    /// silently hide a genuine mismatch.
    #[test]
    fn present_but_invalid_signature_is_rejected_in_every_profile() {
        let image_path = temp_file_path("tampered-process.exe");
        let sig_path = PathBuf::from(format!("{}.sig", image_path.display()));
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        // A valid signature over *different* bytes - structurally fine, wrong image.
        let signature = signing_key.sign(b"some other binary");

        fs::write(&image_path, b"the actual binary").expect("write image");
        fs::write(
            &sig_path,
            base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        )
        .expect("write signature");

        verify_signed_file(&image_path, &signing_key.verifying_key().to_bytes())
            .expect_err("a mismatched signature must be rejected");

        let _ = fs::remove_file(&image_path);
        let _ = fs::remove_file(&sig_path);
    }
}
