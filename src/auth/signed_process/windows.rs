use crate::auth::signed_process::verify_signed_file;
use anyhow::Context;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

pub(crate) fn verify_process(pid: u32, public_key: &[u8]) -> anyhow::Result<()> {
    let handle = ProcessHandle::open(pid)?;
    let _created_at = handle.creation_time()?;
    let image_path = handle.image_path()?;
    verify_signed_file(&image_path, public_key)
}

struct ProcessHandle(HANDLE);

impl ProcessHandle {
    fn open(pid: u32) -> anyhow::Result<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .with_context(|| format!("failed to open process {pid}"))?;
        Ok(Self(handle))
    }

    fn image_path(&self) -> anyhow::Result<PathBuf> {
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
        .context("failed to query process image path")?;
        Ok(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }

    fn creation_time(&self) -> anyhow::Result<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(self.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            .context("failed to query process times")?;
        Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
