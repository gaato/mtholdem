//! Smoke-tests the installed C header against the compiled C ABI library.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn cxx_header_compiles_links_and_checks_layout() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let include_dir = manifest_dir.join("include");
    let target_dir = target_profile_dir(&manifest_dir);
    let target_deps_dir = target_dir.join("deps");
    let work_dir = env::temp_dir().join(format!("mtholdem-header-smoke-{}", std::process::id()));
    fs::create_dir_all(&work_dir).expect("create header smoke work dir");

    let source = work_dir.join("header_smoke.cpp");
    let binary = work_dir.join("header_smoke");
    fs::write(&source, HEADER_SMOKE_CPP).expect("write header smoke source");

    let compile_status = Command::new("c++")
        .arg("-std=c++17")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-I")
        .arg(&include_dir)
        .arg(&source)
        .arg("-L")
        .arg(&target_deps_dir)
        .arg(format!("-Wl,-rpath,{}", target_deps_dir.display()))
        .arg("-lmtholdem_ffi")
        .arg("-o")
        .arg(&binary)
        .status()
        .expect("run c++ for header smoke");
    assert!(
        compile_status.success(),
        "C++ header smoke compile/link failed"
    );

    let mut library_path = OsString::new();
    library_path.push(&target_deps_dir);
    if let Some(existing) = env::var_os("LD_LIBRARY_PATH") {
        library_path.push(":");
        library_path.push(existing);
    }
    let run_status = Command::new(&binary)
        .env("LD_LIBRARY_PATH", library_path)
        .status()
        .expect("run header smoke binary");
    assert!(run_status.success(), "C++ header smoke binary failed");
}

fn target_profile_dir(manifest_dir: &Path) -> PathBuf {
    if let Some(target_dir) = env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(target_dir).join("debug");
    }
    manifest_dir
        .parent()
        .expect("crate directory has workspace crates parent")
        .parent()
        .expect("workspace crates directory has root parent")
        .join("target")
        .join("debug")
}

const HEADER_SMOKE_CPP: &str = r#"
#include <cstddef>
#include <cstdint>
#include "mtholdem.h"

static_assert(MTH_ABI_VERSION_MAJOR == 0u);
static_assert(MTH_ABI_VERSION_MINOR == 1u);
static_assert(MTH_ABI_VERSION_PATCH == 0u);

static_assert(MTH_ERROR_SUCCESS == 0);
static_assert(MTH_ERROR_NULL_POINTER == 1);
static_assert(MTH_ERROR_BUFFER_TOO_SMALL == 2);
static_assert(MTH_ERROR_INVALID_ARGUMENT == 3);
static_assert(MTH_ERROR_NO_EFFECT == 4);
static_assert(MTH_ERROR_CONFIGURATION == 10);
static_assert(MTH_ERROR_CODEC == 11);
static_assert(MTH_ERROR_AUTHENTICATION == 12);
static_assert(MTH_ERROR_CRYPTOGRAPHY == 13);
static_assert(MTH_ERROR_RULES == 14);
static_assert(MTH_ERROR_PROTOCOL == 15);
static_assert(MTH_ERROR_RECOVERY == 16);
static_assert(MTH_ERROR_INTERNAL == 255);

static_assert(MTH_ACTION_FOLD == 1u);
static_assert(MTH_ACTION_CHECK == 2u);
static_assert(MTH_ACTION_CALL == 3u);
static_assert(MTH_ACTION_RAISE_TO == 4u);
static_assert(MTH_ACTION_ALL_IN == 5u);

static_assert(MTH_EFFECT_BROADCAST_PUBLIC == 1u);
static_assert(MTH_EFFECT_SEND_PRIVATE == 2u);
static_assert(MTH_EFFECT_PERSIST == 3u);
static_assert(MTH_EFFECT_REQUEST_ACTION == 4u);
static_assert(MTH_EFFECT_SUBMIT_RECOVERY == 5u);
static_assert(MTH_EFFECT_PUBLISH_SHOWDOWN_BUNDLE == 6u);
static_assert(MTH_EFFECT_FINISHED == 7u);

static_assert(MTH_PHASE_KEY_SETUP == 1u);
static_assert(MTH_PHASE_SHUFFLE == 2u);
static_assert(MTH_PHASE_PRIVATE_REVEAL == 3u);
static_assert(MTH_PHASE_HOLE_CARDS_READY == 4u);
static_assert(MTH_PHASE_BETTING == 5u);
static_assert(MTH_PHASE_BOARD_REVEAL == 6u);
static_assert(MTH_PHASE_SHOWDOWN == 7u);
static_assert(MTH_PHASE_COMPLETE == 8u);

int main() {
    if (mth_abi_version_major() != MTH_ABI_VERSION_MAJOR) {
        return 1;
    }
    if (mth_abi_version_minor() != MTH_ABI_VERSION_MINOR) {
        return 2;
    }
    if (mth_abi_version_patch() != MTH_ABI_VERSION_PATCH) {
        return 3;
    }

    MthAbiLayout layout = {
        MTH_ABI_VERSION_MAJOR,
        MTH_ABI_VERSION_MINOR,
        MTH_ABI_VERSION_PATCH,
        sizeof(MthSessionConfig),
        sizeof(MthBuffer),
        sizeof(MthEffect),
        sizeof(MthPeerStatus),
        sizeof(MthAbiLayout),
    };
    if (mth_abi_check_layout(layout) != MTH_ERROR_SUCCESS) {
        return 4;
    }

    MthAbiLayout reported = {};
    if (mth_abi_layout(&reported) != MTH_ERROR_SUCCESS) {
        return 5;
    }
    if (reported.effect_size != sizeof(MthEffect)) {
        return 6;
    }

    layout.effect_size += 1;
    if (mth_abi_check_layout(layout) != MTH_ERROR_INVALID_ARGUMENT) {
        return 7;
    }
    if (mth_abi_layout(nullptr) != MTH_ERROR_NULL_POINTER) {
        return 8;
    }
    return 0;
}
"#;
