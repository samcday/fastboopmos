use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.lock");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set"));
    let lock_path = manifest_dir
        .parent()
        .expect("workspace root")
        .join("Cargo.lock");
    let version = fs::read_to_string(&lock_path)
        .ok()
        .and_then(|lock| resolved_package_version(&lock, "fastboop-bootpro").map(str::to_string))
        .unwrap_or_else(|| {
            let manifest_path = manifest_dir.join("Cargo.toml");
            let manifest = fs::read_to_string(&manifest_path).expect("read package Cargo.toml");
            direct_dependency_version(&manifest, "fastboop-bootpro")
                .expect("fastboop-bootpro dependency present in package Cargo.toml")
                .to_string()
        });
    println!("cargo:rustc-env=FASTBOOP_BOOTPRO_VERSION=fastboop-bootpro {version}");
}

fn direct_dependency_version<'a>(manifest: &'a str, package_name: &str) -> Option<&'a str> {
    let prefix = format!("{package_name} = ");
    for line in manifest.lines() {
        let Some(value) = line.trim().strip_prefix(prefix.as_str()) else {
            continue;
        };
        return Some(value.trim().trim_matches('"'));
    }
    None
}

fn resolved_package_version<'a>(lock: &'a str, package_name: &str) -> Option<&'a str> {
    for package in lock.split("\n[[package]]\n").skip(1) {
        let mut name = None;
        let mut version = None;
        for line in package.lines() {
            if let Some(value) = line.strip_prefix("name = ") {
                name = Some(value.trim_matches('"'));
            } else if let Some(value) = line.strip_prefix("version = ") {
                version = Some(value.trim_matches('"'));
            }
            if name == Some(package_name) {
                return version;
            }
        }
    }
    None
}
