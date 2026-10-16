//! Embeds prebuilt stubs when `PATCHER_STUB_{WINDOWS,LINUX}[_GUI]` point at them
//! (set by `cargo xtask dist`). Without them the creator looks for stubs at runtime.

use std::path::Path;

fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let mut code = String::new();
    for (var, name) in [
        ("PATCHER_STUB_WINDOWS", "STUB_WINDOWS"),
        ("PATCHER_STUB_LINUX", "STUB_LINUX"),
        ("PATCHER_STUB_WINDOWS_GUI", "STUB_WINDOWS_GUI"),
        ("PATCHER_STUB_LINUX_GUI", "STUB_LINUX_GUI"),
    ] {
        println!("cargo:rerun-if-env-changed={var}");
        match std::env::var(var) {
            Ok(p) if Path::new(&p).is_file() => {
                println!("cargo:rerun-if-changed={p}");
                code += &format!("pub static {name}: Option<&[u8]> = Some(include_bytes!({p:?}));\n");
            }
            _ => code += &format!("pub static {name}: Option<&[u8]> = None;\n"),
        }
    }
    std::fs::write(Path::new(&out).join("stubs.rs"), code).unwrap();
}
