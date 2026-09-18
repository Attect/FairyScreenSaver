//! Compiles the WGSL shader sources into SPIR-V at build time using `naga`.
//!
//! Using `naga` keeps the build free of external toolchains (no Vulkan SDK,
//! no `glslangValidator`, no `shaderc`/cmake).  The generated modules land in
//! `OUT_DIR` and are `include_bytes!`-ed by `src/gfx/shader.rs`.

use std::path::{Path, PathBuf};

const SHADERS: &[&str] = &["scene"];

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    embed_icon(&manifest_dir);

    for name in SHADERS {
        let src_path = manifest_dir.join("shaders").join(format!("{name}.wgsl"));
        println!("cargo:rerun-if-changed={}", src_path.display());
        compile(&src_path, &out_dir, name);
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// Attaches `assets/app.ico` to the binary as resource id 1.  A failure here is
/// only cosmetic, so it downgrades to a warning instead of breaking the build.
#[cfg(windows)]
fn embed_icon(manifest_dir: &Path) {
    let rc = manifest_dir.join("assets").join("app.rc");
    if !rc.exists() {
        println!("cargo:warning=assets/app.rc missing; building without an icon");
        return;
    }
    println!("cargo:rerun-if-changed={}", rc.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets").join("app.ico").display()
    );
    let _ = embed_resource::compile(&rc, embed_resource::NONE);
}

#[cfg(not(windows))]
fn embed_icon(_manifest_dir: &Path) {}

fn compile(src_path: &Path, out_dir: &Path, name: &str) {
    let source = std::fs::read_to_string(src_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", src_path.display()));

    let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| {
        let msg = e.emit_to_string_with_path(&source, src_path.to_string_lossy().as_ref());
        panic!("WGSL parse error in {}:\n{msg}", src_path.display());
    });

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    let info = validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("WGSL validation error in {}: {e:?}", src_path.display()));

    let options = naga::back::spv::Options::default();
    let words = naga::back::spv::write_vec(&module, &info, &options, None)
        .unwrap_or_else(|e| panic!("SPIR-V emission failed for {}: {e:?}", src_path.display()));

    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let dst = out_dir.join(format!("{name}.spv"));
    std::fs::write(&dst, &bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", dst.display()));
    println!(
        "cargo:warning=compiled {} -> {} ({} bytes)",
        src_path.display(),
        dst.display(),
        bytes.len()
    );
}
