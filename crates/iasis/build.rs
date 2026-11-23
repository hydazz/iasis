//! The tools carry the IasisOS image named by `IASIS_OS_UF2`, when it is set.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-env-changed=IASIS_OS_UF2");
    let carried = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("IasisOS.uf2");
    match env::var_os("IASIS_OS_UF2") {
        Some(image) => {
            let image = PathBuf::from(image);
            println!("cargo::rerun-if-changed={}", image.display());
            fs::copy(&image, &carried)
                .unwrap_or_else(|error| panic!("IASIS_OS_UF2 {}: {error}", image.display()));
        }
        None => fs::write(&carried, []).expect("writing an empty image"),
    }
}
