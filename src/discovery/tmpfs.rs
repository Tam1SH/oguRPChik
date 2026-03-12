use crate::discovery::kv::KvStore;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

pub struct TmpfsKv {
    base: PathBuf,
}

impl TmpfsKv {
    pub fn new() -> Result<Self> {
        let base = base_path()?;
        fs::create_dir_all(&base)
            .with_context(|| format!("Failed to create base dir: {}", base.display()))?;
        tracing::debug!(base = %base.display(), "TmpfsKv initialised");
        Ok(Self { base })
    }

    fn full_path(&self, key: &str) -> PathBuf {
        let rel = key.replace('\\', "/");
        self.base.join(rel)
    }
}

impl KvStore for TmpfsKv {
    fn write(&self, key: &str, value: &str) -> Result<()> {
        let path = self.full_path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create dir: {}", parent.display()))?;
        }
        tracing::debug!(key, path = %path.display(), "tmpfs write");
        fs::write(&path, value)
            .with_context(|| format!("Failed to write key '{key}'"))?;
        Ok(())
    }

    fn read(&self, key: &str) -> Result<String> {
        let path = self.full_path(key);
        tracing::debug!(key, path = %path.display(), "tmpfs read");
        fs::read_to_string(&path)
            .with_context(|| format!("Failed to read key '{key}'"))
    }

    fn watch(&self, key: &str) -> Result<kanal::Receiver<()>> {
        use inotify::{EventMask, Inotify, WatchMask};

        let path = self.full_path(key);
        let parent = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.base.clone());

        tracing::debug!(key, path = %path.display(), "tmpfs watch started");

        fs::create_dir_all(&parent)
            .with_context(|| format!("Failed to create watch dir: {}", parent.display()))?;

        let file_name = path
            .file_name()
            .map(|n| n.to_owned())
            .with_context(|| format!("Key has no file component: '{key}'"))?;

        let (tx, rx) = kanal::unbounded::<()>();

        std::thread::spawn(move || {
            let mut inotify = match Inotify::init() {
                Ok(i) => i,
                Err(e) => {
                    tracing::error!("inotify init failed: {e}");
                    return;
                }
            };

            if let Err(e) = inotify.watches().add(
                &parent,
                WatchMask::CREATE | WatchMask::MODIFY | WatchMask::MOVED_TO,
            ) {
                tracing::error!(path = %parent.display(), "inotify add watch failed: {e}");
                return;
            }

            if path.exists() {
                tracing::debug!(?file_name, "watch: file already exists, signalling");
                if tx.send(()).is_err() {
                    return;
                }
            }

            let mut buffer = [0u8; 4096];
            loop {
                let events = match inotify.read_events_blocking(&mut buffer) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!("inotify read_events failed: {e}");
                        break;
                    }
                };

                for event in events {
                    let matches = event
                        .name
                        .map(|n| n == file_name)
                        .unwrap_or(false);

                    if matches
                        && event
                            .mask
                            .intersects(EventMask::CREATE | EventMask::MODIFY | EventMask::MOVED_TO)
                    {
                        tracing::debug!(?file_name, "watch: inotify event fired");
                        if tx.send(()).is_err() {
                            tracing::debug!(?file_name, "watch: receiver dropped, stopping");
                            return;
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.full_path(key);
        tracing::debug!(key, path = %path.display(), "tmpfs delete");

        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("Failed to delete dir for key '{key}'"))?;
        } else if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete file for key '{key}'"))?;
        }
        Ok(())
    }
}

fn base_path() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));

    if !runtime.exists() {
        bail!("Runtime dir does not exist: {}", runtime.display());
    }

    Ok(runtime.join("ogurpchik"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn make_kv(test_name: &str) -> TmpfsKv {
        let runtime = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));

        let base = runtime
            .join("ogurpchik_test")
            .join(test_name);

        fs::create_dir_all(&base).expect("failed to create test dir");

        TmpfsKv { base }
    }

    impl Drop for TmpfsKv {
        fn drop(&mut self) {

            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn write_and_read_roundtrip() {
        let kv = make_kv("write_and_read_roundtrip");
        kv.write("Services\\foo", "hello").unwrap();
        assert_eq!(kv.read("Services\\foo").unwrap(), "hello");
    }

    #[test]
    fn overwrite_returns_latest_value() {
        let kv = make_kv("overwrite_returns_latest_value");
        kv.write("k", "v1").unwrap();
        kv.write("k", "v2").unwrap();
        assert_eq!(kv.read("k").unwrap(), "v2");
    }

    #[test]
    fn read_missing_key_returns_error() {
        let kv = make_kv("read_missing_key_returns_error");
        let result = kv.read("Services\\does_not_exist");
        assert!(result.is_err());
    }

    #[test]
    fn delete_file_key() {
        let kv = make_kv("delete_file_key");
        kv.write("to_delete", "val").unwrap();
        kv.delete("to_delete").unwrap();
        assert!(kv.read("to_delete").is_err());
    }

    #[test]
    fn delete_nested_key() {
        let kv = make_kv("delete_nested_key");
        kv.write("Services\\svc\\endpoint", "127.0.0.1:9000").unwrap();

        kv.delete("Services\\svc").unwrap();
        assert!(kv.read("Services\\svc\\endpoint").is_err());
    }

    #[test]
    fn delete_nonexistent_key_is_ok() {
        let kv = make_kv("delete_nonexistent_key_is_ok");

        kv.delete("ghost").unwrap();
    }

    #[test]
    fn nested_key_creates_intermediate_dirs() {
        let kv = make_kv("nested_key_creates_intermediate_dirs");
        kv.write("a\\b\\c\\d", "deep").unwrap();
        assert_eq!(kv.read("a\\b\\c\\d").unwrap(), "deep");
    }

    #[test]
    fn sibling_keys_are_independent() {
        let kv = make_kv("sibling_keys_are_independent");
        kv.write("Services\\alpha", "1").unwrap();
        kv.write("Services\\beta", "2").unwrap();
        assert_eq!(kv.read("Services\\alpha").unwrap(), "1");
        assert_eq!(kv.read("Services\\beta").unwrap(), "2");
    }

    #[test]
    fn watch_fires_on_write() {
        let kv = Arc::new(make_kv("watch_fires_on_write"));
        let rx = kv.watch("Services\\late_writer").unwrap();

        let kv2 = kv.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            kv2.write("Services\\late_writer", "payload").unwrap();
        });

        let signal = rx.recv_timeout(Duration::from_secs(2));
        assert!(signal.is_ok(), "watch did not fire within timeout");
    }

    #[test]
    fn watch_fires_immediately_when_key_exists() {
        let kv = Arc::new(make_kv("watch_fires_immediately_when_key_exists"));
        kv.write("Services\\existing", "already_here").unwrap();

        let rx = kv.watch("Services\\existing").unwrap();
        let signal = rx.recv_timeout(Duration::from_secs(1));
        assert!(signal.is_ok(), "watch did not signal for pre-existing key");
    }

    #[test]
    fn watch_fires_on_overwrite() {
        let kv = Arc::new(make_kv("watch_fires_on_overwrite"));
        kv.write("Services\\svc", "v1").unwrap();

        let rx = kv.watch("Services\\svc").unwrap();

        rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let kv2 = kv.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            kv2.write("Services\\svc", "v2").unwrap();
        });

        let signal = rx.recv_timeout(Duration::from_secs(2));
        assert!(signal.is_ok(), "watch did not fire on overwrite");
    }

    #[test]
    fn watch_thread_stops_when_receiver_dropped() {
        let kv = Arc::new(make_kv("watch_thread_stops_when_receiver_dropped"));
        let rx = kv.watch("Services\\ephemeral").unwrap();
        drop(rx);

        std::thread::sleep(Duration::from_millis(100));

        kv.write("Services\\ephemeral", "late").unwrap();
    }

    #[test]
    fn concurrent_writes_to_different_keys() {
        let kv = Arc::new(make_kv("concurrent_writes_to_different_keys"));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let kv = kv.clone();
                std::thread::spawn(move || {
                    let key = format!("Services\\worker_{i}");
                    kv.write(&key, &i.to_string()).unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        for i in 0..8u32 {
            let val = kv.read(&format!("Services\\worker_{i}")).unwrap();
            assert_eq!(val, i.to_string());
        }
    }

    #[test]
    fn scope_internal_returns_tmpfs_kv_on_linux() {
        let kv = Scope::Internal.into_kv().expect("Scope::Internal failed");

        kv.write("Scope\\test_key", "ok").unwrap();
        assert_eq!(kv.read("Scope\\test_key").unwrap(), "ok");
        kv.delete("Scope\\test_key").unwrap();
    }
}
