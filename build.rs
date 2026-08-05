//! Code generation for the `.capnp` schemas in `schema/`.
//!
//! The `capnpc` crate shells out to the external C++ `capnp` binary — there
//! is no pure-Rust equivalent (unlike `protox` for protobuf). The binary
//! must be installed separately; this is a build requirement for this crate
//! and for every plugin author, and is documented in the README.

use std::path::{Path, PathBuf};

fn main() {
    let mut cmd = capnpc::CompilerCommand::new();
    cmd.src_prefix("schema")
        .file("schema/agent.capnp")
        .file("schema/plugin.capnp");

    // `-> stream` methods import "/capnp/stream.capnp" from the Cap'n Proto
    // C++ distribution. capnpc does not ship those stdlib files, and some
    // installations (notably the Windows winget package) don't put them
    // where the `capnp` binary discovers them, so point capnpc at an
    // include dir explicitly. Resolution order:
    //
    // 1. `CAPNP_INCLUDE_DIR` env var — the documented escape hatch.
    // 2. Standard Unix locations (distro/homebrew packages).
    // 3. The winget package layout on Windows.
    for path in capnp_include_candidates() {
        if path.join("capnp").join("stream.capnp").exists() {
            cmd.import_path(path);
        }
    }

    println!("cargo:rerun-if-changed=schema/agent.capnp");
    println!("cargo:rerun-if-changed=schema/plugin.capnp");
    println!("cargo:rerun-if-env-changed=CAPNP_INCLUDE_DIR");

    cmd.run().expect(
        "capnp schema compilation failed; is the `capnp` binary installed \
         (and, for -> stream support, CAPNP_INCLUDE_DIR pointing at the dir \
         containing capnp/stream.capnp)?",
    );
}

fn capnp_include_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(dir) = std::env::var_os("CAPNP_INCLUDE_DIR") {
        candidates.push(PathBuf::from(dir));
    }

    candidates.push(PathBuf::from("/usr/include"));
    candidates.push(PathBuf::from("/usr/local/include"));
    candidates.push(PathBuf::from("/opt/homebrew/include"));

    // winget layout: %LOCALAPPDATA%\Microsoft\WinGet\Packages\
    //   capnproto.capnproto_*\capnproto-c++-<version>\src
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let packages = Path::new(&local)
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        if let Ok(entries) = std::fs::read_dir(&packages) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if !name.starts_with("capnproto.capnproto") {
                    continue;
                }
                if let Ok(inner) = std::fs::read_dir(entry.path()) {
                    for sub in inner.flatten() {
                        let sub_name = sub.file_name();
                        let Some(sub_name) = sub_name.to_str() else { continue };
                        if sub_name.starts_with("capnproto-c++") {
                            candidates.push(sub.path().join("src"));
                        }
                    }
                }
            }
        }
    }

    candidates
}
