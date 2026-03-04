use anyhow::{Result, bail};

pub trait KvStore: Send + Sync + 'static {
    fn write(&self, key: &str, value: &str) -> Result<()>;
    fn read(&self, key: &str) -> Result<String>;
    fn watch(&self, key: &str) -> Result<flume::Receiver<()>>;
    fn delete(&self, key: &str) -> Result<()>;
}

pub struct RegistryKv {
    base: String,
}

fn reg_exe() -> &'static str {
    if cfg!(unix) {
        "/mnt/c/Windows/System32/reg.exe"
    } else {
        "reg.exe"
    }
}

impl RegistryKv {
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let base = base.into();

        let out = std::process::Command::new(reg_exe())
            .args(["add", &base, "/f"])
            .output()?;

        if !out.status.success() {
            bail!(
                "Failed to create base registry key '{}': {}",
                base,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        Ok(Self { base })
    }

    fn full_path(&self, key: &str) -> String {
        format!("{}\\{}", self.base, key)
    }
}

#[cfg(target_os = "windows")]
impl KvStore for RegistryKv {
    fn write(&self, key: &str, value: &str) -> Result<()> {
        use winreg::RegKey;
        use winreg::enums::*;

        let path = self.full_path(key);
        let path = path.trim_start_matches("HKCU\\");
        tracing::debug!(key = path, "registry write");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (subkey, _) = hkcu.create_subkey(path)?;
        subkey.set_value("value", &value.to_string())?;
        Ok(())
    }

    fn read(&self, key: &str) -> Result<String> {
        use winreg::RegKey;
        use winreg::enums::*;

        let path = self.full_path(key);
        let path = path.trim_start_matches("HKCU\\");
        tracing::debug!(key = path, "registry read");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let subkey = hkcu.open_subkey(path)?;
        Ok(subkey.get_value("value")?)
    }

    fn watch(&self, key: &str) -> Result<flume::Receiver<()>> {
        use windows::Win32::System::Registry::{
            HKEY, REG_NOTIFY_CHANGE_LAST_SET, RegNotifyChangeKeyValue,
        };
        use winreg::RegKey;
        use winreg::enums::*;

        let path = self.full_path(key);
        let path_owned = path.trim_start_matches("HKCU\\").to_string();
        tracing::debug!(key = %path_owned, "registry watch started");
        let (tx, rx) = flume::unbounded();

        std::thread::spawn(move || {
            let key = loop {
                let hkcu = RegKey::predef(HKEY_CURRENT_USER);
                match hkcu.open_subkey(&path_owned) {
                    Ok(k) => {
                        tracing::debug!(key = %path_owned, "registry watch key found");
                        let _ = tx.send(());
                        break k;
                    }
                    Err(e) if e.raw_os_error() == Some(2) => {
                        tracing::trace!(key = %path_owned, "registry watch key not yet, retrying");
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    Err(e) => {
                        tracing::error!(key = %path_owned, error = %e, "watch: fatal error opening key");
                        return;
                    }
                }
            };

            loop {
                unsafe {
                    let _ = RegNotifyChangeKeyValue(
                        HKEY(key.raw_handle() as _),
                        false,
                        REG_NOTIFY_CHANGE_LAST_SET,
                        None,
                        false,
                    );
                }
                tracing::debug!(key = %path_owned, "registry key changed");
                if tx.send(()).is_err() {
                    tracing::debug!(key = %path_owned, "watch receiver dropped, stopping");
                    break;
                }
            }
        });

        Ok(rx)
    }

    fn delete(&self, key: &str) -> Result<()> {
        use winreg::RegKey;
        use winreg::enums::*;
        let path = self.full_path(key);
        let path = path.trim_start_matches("HKCU\\");
        tracing::debug!(key = path, "registry delete");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        hkcu.delete_subkey_all(path)?;
        Ok(())
    }
}

#[cfg(unix)]
impl KvStore for RegistryKv {
    fn write(&self, key: &str, value: &str) -> Result<()> {
        let path = self.full_path(key);
        tracing::debug!(key = %path, "registry write via reg.exe");
        let out = std::process::Command::new(reg_exe())
            .args([
                "add", &path, "/v", "value", "/t", "REG_SZ", "/d", value, "/f",
            ])
            .output()?;

        if !out.status.success() {
            bail!(
                "reg.exe add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    fn read(&self, key: &str) -> Result<String> {
        let path = self.full_path(key);
        tracing::debug!(key = %path, "registry read via reg.exe");
        let out = std::process::Command::new(reg_exe())
            .args(["query", &path, "/v", "value"])
            .output()?;

        if !out.status.success() {
            bail!(
                "reg.exe query failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let stdout = String::from_utf8(out.stdout)?;
        stdout
            .lines()
            .find(|l| l.contains("REG_SZ"))
            .and_then(|l| l.splitn(3, "REG_SZ").nth(1))
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("value not found in key '{}'", path))
    }

    fn watch(&self, key: &str) -> Result<flume::Receiver<()>> {
        let path = self.full_path(key);
        tracing::debug!(key = %path, "registry watch started via reg.exe polling");
        let (tx, rx) = flume::unbounded();

        std::thread::spawn(move || {
            let mut last: Option<String> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(200));

                let out = std::process::Command::new(reg_exe())
                    .args(["query", &path, "/v", "value"])
                    .output();

                let out = match out {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::error!(key = %path, error = %e, "watch: failed to spawn reg.exe");
                        break;
                    }
                };

                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if out.stdout.is_empty()
                        || stderr.contains("unable to find")
                        || stderr.contains("ERROR_FILE_NOT_FOUND")
                    {
                        // key may not exist yet, retry
                        tracing::trace!(key = %path, "watch: key not yet, retrying");
                        continue;
                    }
                    tracing::error!(key = %path, error = %stderr, "watch: reg.exe fatal error");
                    break;
                }

                let current = String::from_utf8(out.stdout).ok().and_then(|s| {
                    s.lines()
                        .find(|l| l.contains("REG_SZ"))
                        .and_then(|l| l.splitn(3, "REG_SZ").nth(1))
                        .map(|v| v.trim().to_string())
                });

                if current != last {
                    tracing::debug!(key = %path, "watch: key changed");
                    last = current;
                    if tx.send(()).is_err() {
                        tracing::debug!(key = %path, "watch: receiver dropped, stopping");
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.full_path(key);
        tracing::debug!(key = %path, "registry delete via reg.exe");
        let out = std::process::Command::new(reg_exe())
            .args(["delete", &path, "/f"])
            .output()?;
        if !out.status.success() {
            bail!(
                "reg.exe delete failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }
}

pub fn default_kv() -> Result<RegistryKv> {
    RegistryKv::new("HKCU\\Software\\Ogurpchik")
}
