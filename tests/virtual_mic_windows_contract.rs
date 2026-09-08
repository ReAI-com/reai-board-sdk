//! Windows virtual-mic wire-contract guards.
//!
//! The Rust backend ([`crate`]'s `src/virtual_mic/windows.rs`) and the WDK
//! driver (`virtual-mic/driver-windows/`) must agree on the control device
//! name, the root hardware ID, the package file names, and the endpoint
//! friendly name. These tests read the driver sources directly so a one-sided
//! rename fails CI instead of failing on a user's machine.
//!
//! Whole file is Windows+`virtual-mic` only (the other platforms never
//! compile the Windows backend). The driver source checks additionally skip
//! when `virtual-mic/driver-windows/` is absent, e.g. inside the crates.io
//! package where it is excluded.

#![cfg(all(feature = "virtual-mic", target_os = "windows"))]

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn driver_dir() -> Option<PathBuf> {
    let dir = manifest_dir().join("virtual-mic").join("driver-windows");
    dir.is_dir().then_some(dir)
}

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path.as_ref())
        .unwrap_or_else(|e| panic!("read {}: {e}", path.as_ref().display()))
}

#[test]
fn control_device_name_matches_driver() {
    const RUST_OPEN_PATH: &str = r#"\\.\ReAIVibeBoardVirtualMic"#;
    // C source text: the literal in vbmcontrol.h has escaped backslashes.
    const DRIVER_DEVICE_NAME: &str = r#"\\Device\\ReAIVibeBoardVirtualMic"#;

    assert!(
        read(manifest_dir().join("src/virtual_mic/windows.rs")).contains(RUST_OPEN_PATH),
        "Rust backend no longer opens {RUST_OPEN_PATH}"
    );
    let Some(dir) = driver_dir() else { return };
    assert!(
        read(dir.join("Source/Main/vbmcontrol.h")).contains(DRIVER_DEVICE_NAME),
        "driver control device no longer names itself {DRIVER_DEVICE_NAME}"
    );
}

#[test]
fn inf_installs_expected_root_device_and_package() {
    let Some(dir) = driver_dir() else { return };
    let inf = read(dir.join("Source/Main/ReAIVibeBoardVirtualMic.inx"));

    assert!(inf.contains("ROOT\\ReAIVB"), "INF hardware id moved");
    assert!(
        inf.contains("CatalogFile = ReAIVibeBoardVirtualMic.cat"),
        "INF catalog file name moved"
    );
    assert!(
        inf.contains("ReAIVibeBoardVirtualMic.sys"),
        "INF driver file name moved"
    );
    assert!(
        inf.contains("AddService=ReAIVibeBoardVirtualMic"),
        "INF service name moved"
    );

    // The SDK's documented package layout (REAI_VIRTUAL_MIC_DRIVER_DIR).
    assert!(
        dir.join("Source/Main")
            .join("ReAIVibeBoardVirtualMic.inx")
            .is_file(),
        "INF template moved"
    );
}

#[test]
fn endpoint_friendly_name_matches_sdk_device_name() {
    // What apps see in sound settings must stay equal to
    // `virtual_mic::DEVICE_NAME` (the cross-platform contract from issue #11).
    assert_eq!(reai_board_sdk::virtual_mic::DEVICE_NAME, "ReAI-Vibe-Board");

    let Some(dir) = driver_dir() else { return };
    let inf = read(dir.join("Source/Main/ReAIVibeBoardVirtualMic.inx"));
    assert!(
        inf.contains("REAIVBMIC.WaveMicArray1.szPname=\"ReAI-Vibe-Board\""),
        "INF endpoint friendly name no longer ReAI-Vibe-Board"
    );
}
