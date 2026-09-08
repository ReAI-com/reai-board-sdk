//! Build script.
//!
//! Most of the crate needs nothing from the build system. The one exception
//! is the macOS-only `virtual-mic` feature: it compiles the bundled CoreAudio
//! HAL plugin (vendored libASPL + ReAI driver under `virtual-mic/`) with
//! cmake and exposes the produced bundle path to the crate through the
//! `REAI_VIRTUAL_MIC_DRIVER` env var. cmake is therefore only required when
//! that feature is enabled (`brew install cmake`).

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let virtual_mic = env::var_os("CARGO_FEATURE_VIRTUAL_MIC").is_some();
    let macos = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if !virtual_mic || !macos {
        return;
    }

    println!("cargo:rerun-if-changed=virtual-mic");
    println!("cargo:rerun-if-env-changed=REAI_VIRTUAL_MIC_CODESIGN_ID");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let driver_src = manifest_dir.join("virtual-mic/driver");
    let build_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("driver-build");

    // The SDK ships the driver ad-hoc signed (enough to load, see the
    // virtual-mic docs). Product developers distributing it sign it with
    // their own Developer ID certificate via this variable. Always pass the
    // variable explicitly (empty default) — CMake cache variables persist
    // across reconfigures, so omitting it would silently keep a stale ID.
    let mut configure_args = vec![
        String::from("-S"),
        driver_src.display().to_string(),
        String::from("-B"),
        build_dir.display().to_string(),
        String::from("-DCMAKE_BUILD_TYPE=Release"),
        String::from("-DCODESIGN_ID="),
    ];
    configure_args[5].push_str(&env::var("REAI_VIRTUAL_MIC_CODESIGN_ID").unwrap_or_default());

    let status = Command::new("cmake")
        .args(&configure_args)
        .status()
        .expect("cmake not found: the `virtual-mic` feature requires cmake (brew install cmake)");
    assert!(
        status.success(),
        "cmake configure failed for virtual-mic driver"
    );

    let status = Command::new("cmake")
        .arg("--build")
        .arg(&build_dir)
        .arg("--config")
        .arg("Release")
        .arg("--parallel")
        .status()
        .expect("failed to run cmake build");
    assert!(
        status.success(),
        "cmake build failed for virtual-mic driver"
    );

    let bundle = build_dir.join("ReAIVibeBoard.driver");
    assert!(
        bundle.exists(),
        "driver bundle missing after cmake build: {}",
        bundle.display()
    );
    println!(
        "cargo:rustc-env=REAI_VIRTUAL_MIC_DRIVER={}",
        bundle.display()
    );
}
