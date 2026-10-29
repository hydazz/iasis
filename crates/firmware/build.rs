use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("memory.x"), include_bytes!("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-env-changed=IASIS_USB_VID");
    println!("cargo:rerun-if-env-changed=IASIS_USB_PID");
    let (vid, pid) = match (env::var("IASIS_USB_VID"), env::var("IASIS_USB_PID")) {
        (Ok(vid), Ok(pid)) => (hex(&vid), hex(&pid)),
        (Err(_), Err(_)) => (0x1209, 0x0001),
        _ => panic!("set both IASIS_USB_VID and IASIS_USB_PID"),
    };
    let version: Vec<u16> = env::var("CARGO_PKG_VERSION")
        .unwrap()
        .split('.')
        .map(|part| part.parse().unwrap())
        .collect();
    fs::write(out.join("config.rs"), format!(
        "const USB_VID: u16 = {vid};\nconst USB_PID: u16 = {pid};\nconst FIRMWARE_VERSION: [u16; 3] = {version:?};\n"
    )).unwrap();
}

fn hex(value: &str) -> u16 {
    u16::from_str_radix(value.trim_start_matches("0x"), 16)
        .expect("USB ID must be a hexadecimal u16")
}
