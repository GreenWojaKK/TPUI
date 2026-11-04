//! Embeds the icon resource in the Windows executable, which Explorer, shortcuts, the window and the taskbar use, and
//! builds the code core next to the viewer's executable.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("compile icon resource");
    build_code_core();
}

/// Builds the Go code core. A failed build only warns: the viewer works without the core and shows source files as plain
/// text.
fn build_code_core() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source = manifest.join("../../core");
    println!("cargo:rerun-if-changed={}", source.display());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    // OUT_DIR is <profile>/build/<package>/out, and the executable goes in <profile>.
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        println!("cargo:warning=cannot tell where to put the code core");
        return;
    };
    if let Err(reason) = run_go_build(&source, profile_dir) {
        println!("cargo:warning=the code core was not built: {reason}");
    }
}

fn run_go_build(source: &Path, profile_dir: &Path) -> Result<(), String> {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let suffix = if target_os == "windows" { ".exe" } else { "" };
    let output = Command::new("go")
        .current_dir(source)
        .env("CGO_ENABLED", "0")
        .args(["build", "-trimpath", "-ldflags=-s -w", "-o"])
        .arg(profile_dir.join(format!("tpui-core{suffix}")))
        .arg(".")
        .output()
        .map_err(|error| format!("cannot run go: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .replace('\n', " "))
    }
}
