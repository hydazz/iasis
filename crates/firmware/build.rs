use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("memory.x"), include_bytes!("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-arg-bin=iasis-firmware=--build-id=sha1");
    let version = ["MAJOR", "MINOR", "PATCH"].map(|part| {
        env::var(format!("CARGO_PKG_VERSION_{part}"))
            .unwrap()
            .parse::<u16>()
            .unwrap()
    });
    fs::write(
        out.join("config.rs"),
        format!("const FIRMWARE_VERSION: [u16; 3] = {version:?};\n"),
    )
    .unwrap();
}
