//! Links clang's compiler-rt builtins (`libclang_rt.osx.a`) into Apple-silicon macOS builds.
//!
//! The prebuilt ONNX Runtime archive's CoreML objects call `__isPlatformVersionAtLeast` (the
//! `@available` check). Without LTO, std's copy of that function satisfies them; with `lto = "fat"`
//! or `"thin"` LTO internalizes it, and rustc links with `-nodefaultlibs`, so the toolchain's
//! builtins archive is never searched and the link fails. Naming the archive here puts it on the
//! final link of every binary that depends on this crate. It is not bundled into the rlib, and the
//! linker takes a member from it only when no other object defines the symbol.
//!
//! Every other target, Intel macOS and iOS included, gets nothing from this script.

use std::env;
use std::path::Path;
use std::process::Command;

const ARCHIVE: &str = "libclang_rt.osx.a";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os != "macos" || arch != "aarch64" {
        return;
    }
    match builtins_dir() {
        Some(dir) => {
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static:-bundle=clang_rt.osx");
        }
        None => println!(
            "cargo:warning=frust-onnx: `clang --print-file-name={ARCHIVE}` found no archive; \
             an LTO release build may fail to link `___isPlatformVersionAtLeast` \
             (install the Xcode command-line tools)"
        ),
    }
}

/// The directory holding the builtins archive of the `clang` on `PATH`, if it has one.
fn builtins_dir() -> Option<String> {
    let output = Command::new("clang")
        .arg(format!("--print-file-name={ARCHIVE}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let printed = String::from_utf8(output.stdout).ok()?;
    // Clang echoes the bare name back when it cannot find the file.
    let archive = Path::new(printed.trim());
    if !archive.is_absolute() || !archive.is_file() {
        return None;
    }
    archive.parent()?.to_str().map(str::to_owned)
}
