//! The build ID: the version and a hash of every source file in the
//! workspace. Two binaries built from the same sources agree on it, and any
//! change to a source makes a new one, so a client always notices a daemon
//! from another build.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let root = manifest.join("../..");
    let mut files = Vec::new();
    collect(&root.join("crates"), &mut files);
    for top in ["Cargo.toml", "Cargo.lock"] {
        files.push(root.join(top));
    }
    files.sort();

    let mut hasher = DefaultHasher::new();
    for file in &files {
        if let Ok(bytes) = std::fs::read(file) {
            hasher.write(
                file.strip_prefix(&root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .as_bytes(),
            );
            hasher.write(&bytes);
        }
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    println!(
        "cargo:rustc-env=TWYX_BUILD_ID={version}-{:016x}",
        hasher.finish()
    );
    println!("cargo:rerun-if-changed={}", root.join("crates").display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.lock").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.toml").display()
    );
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "rs" || extension == "toml")
        {
            files.push(path);
        }
    }
}
