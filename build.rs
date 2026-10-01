// Lets this crate's tests run without manual setup when the `shim` feature
// links the Noesis library. noesis_runtime's build script publishes the
// resolved Bin/<platform> path as DEP_NOESIS_LIB_DIR (`links = "Noesis"`); it is
// unset without `shim` and under DOCS_RS, when nothing links Noesis.
//
// Linux bakes that path into the rpath. Windows has no rpath, so Noesis.dll is
// copied next to the test binaries instead.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_NOESIS_LIB_DIR");

    let Ok(lib_dir) = env::var("DEP_NOESIS_LIB_DIR") else {
        return;
    };

    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
        }
        Ok("windows") => {
            // OUT_DIR is <target>/<profile>/build/<pkg>-<hash>/out; three levels
            // up is the profile dir holding the binaries and deps/.
            let dll = Path::new(&lib_dir).join("Noesis.dll");
            let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
            if let Some(profile_dir) = out_dir.ancestors().nth(3) {
                for sub in ["", "deps"] {
                    let dest = profile_dir.join(sub);
                    if dest.is_dir() {
                        // Best effort: PATH remains a fallback.
                        let _ = std::fs::copy(&dll, dest.join("Noesis.dll"));
                    }
                }
            }
        }
        _ => {}
    }
}
