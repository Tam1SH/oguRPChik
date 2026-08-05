use crate::auth::signed_process::verify_signed_file;
use crate::error::{HandshakeError, Result};
use error_stack::ResultExt;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

/// Creation time of the process `pid` (as a `FILETIME` value). Read this
/// *before* verifying the image signature and again after: if the process
/// dies and its PID is recycled mid-check, the value changes and the caller
/// fails closed.
pub(crate) fn process_creation_time(pid: u32) -> Result<u64, HandshakeError> {
    ProcessHandle::open(pid)?.creation_time()
}

/// Verifies that the image of process `pid` is signed by `public_key`.
pub(crate) fn verify_process_image(pid: u32, public_key: &[u8]) -> Result<(), HandshakeError> {
    let handle = ProcessHandle::open(pid)?;
    let image_path = handle.image_path()?;
    verify_signed_file(&image_path, public_key)
}

struct ProcessHandle(HANDLE);

impl ProcessHandle {
    fn open(pid: u32) -> Result<Self, HandshakeError> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .change_context(HandshakeError::SignedProcessVerificationFailed)
            .attach(format!("failed to open process {pid}"))?;
        Ok(Self(handle))
    }

    fn image_path(&self) -> Result<PathBuf, HandshakeError> {
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .change_context(HandshakeError::SignedProcessVerificationFailed)
        .attach("failed to query process image path")?;
        Ok(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }

    fn creation_time(&self) -> Result<u64, HandshakeError> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(self.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            .change_context(HandshakeError::SignedProcessVerificationFailed)
            .attach("failed to query process times")?;
        Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
