use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.lock");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set"));
    let manifest_path = manifest_dir.join("Cargo.toml");
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_path)
        .exec()
        .expect("read cargo metadata");

    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "fastboop-bootpro")
        .expect("fastboop-bootpro dependency present in cargo metadata");
    println!(
        "cargo:rustc-env=FASTBOOP_BOOTPRO_VERSION=fastboop-bootpro {}",
        package.version
    );
}
