//! Smoke-tests the installed C header against the compiled C ABI library.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn cxx_header_compiles_links_and_checks_layout() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let include_dir = manifest_dir.join("include");
    let target_dir = target_profile_dir();
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

fn target_profile_dir() -> PathBuf {
    let test_binary = env::current_exe().expect("resolve header smoke test executable");
    let deps_dir = test_binary
        .parent()
        .expect("header smoke test executable has a parent directory");
    assert_eq!(
        deps_dir.file_name().and_then(|name| name.to_str()),
        Some("deps"),
        "header smoke test executable is not under a Cargo profile deps directory"
    );
    deps_dir
        .parent()
        .expect("Cargo profile deps directory has a profile parent")
        .to_path_buf()
}

const HEADER_SMOKE_CPP: &str = r#"
#include <cstddef>
#include <cstdint>
#include <cstring>
#include "mtholdem.h"

struct LegacyMthAbiLayoutV1 {
    uint32_t version_major;
    uint32_t version_minor;
    uint32_t version_patch;
    size_t session_config_size;
    size_t table_config_size;
    size_t buffer_size;
    size_t effect_size;
    size_t table_event_size;
    size_t peer_status_size;
    size_t abi_layout_size;
};

extern "C" int32_t mth_abi_layout(LegacyMthAbiLayoutV1 *out_layout);
extern "C" int32_t mth_abi_check_layout(LegacyMthAbiLayoutV1 layout);

static_assert(MTH_ABI_VERSION_MAJOR == 0u);
static_assert(MTH_ABI_VERSION_MINOR == 2u);
static_assert(MTH_ABI_VERSION_PATCH == 0u);
static_assert(MTH_IDENTITY_SEED_BYTES == 32u);
static_assert(MTH_IDENTITY_PUBLIC_KEY_BYTES == 32u);

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

static_assert(MTH_PEER_EVENT_HAND_STARTED == 1u);
static_assert(MTH_PEER_EVENT_LOCAL_HOLE_CARDS_OPENED == 2u);
static_assert(MTH_PEER_EVENT_BETTING_ACTION_ACCEPTED == 3u);
static_assert(MTH_PEER_EVENT_BOARD_STREET_OPENED == 4u);
static_assert(MTH_PEER_EVENT_SHOWDOWN_HAND_OPENED == 5u);
static_assert(MTH_PEER_EVENT_HAND_FINISHED == 6u);

static_assert(MTH_PHASE_KEY_SETUP == 1u);
static_assert(MTH_PHASE_SHUFFLE == 2u);
static_assert(MTH_PHASE_PRIVATE_REVEAL == 3u);
static_assert(MTH_PHASE_HOLE_CARDS_READY == 4u);
static_assert(MTH_PHASE_BETTING == 5u);
static_assert(MTH_PHASE_BOARD_REVEAL == 6u);
static_assert(MTH_PHASE_SHOWDOWN == 7u);
static_assert(MTH_PHASE_COMPLETE == 8u);

static_assert(MTH_TABLE_BACKEND_PEER == 1u);
static_assert(MTH_TABLE_BACKEND_RULES_ONLY == 2u);

static_assert(MTH_TABLE_EVENT_HAND_STARTED == 1u);
static_assert(MTH_TABLE_EVENT_HOLE_CARDS == 2u);
static_assert(MTH_TABLE_EVENT_ACTION_REQUEST == 3u);
static_assert(MTH_TABLE_EVENT_ACTION_TAKEN == 4u);
static_assert(MTH_TABLE_EVENT_BOARD_OPENED == 5u);
static_assert(MTH_TABLE_EVENT_SHOWDOWN == 6u);
static_assert(MTH_TABLE_EVENT_HAND_FINISHED == 7u);
static_assert(MTH_TABLE_EVENT_SESSION_FINISHED == 8u);

static_assert(MTH_BOT_PRESET_NIT == 1u);
static_assert(MTH_BOT_PRESET_BALANCED == 2u);
static_assert(MTH_BOT_PRESET_LOOSE_PASSIVE == 3u);
static_assert(MTH_BOT_PRESET_LAG == 4u);
static_assert(MTH_BOT_PRESET_MANIAC == 5u);

static_assert(offsetof(MthAbiLayout, peer_event_size) == sizeof(LegacyMthAbiLayoutV1));

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
        sizeof(MthTableConfig),
        sizeof(MthBuffer),
        sizeof(MthEffect),
        sizeof(MthTableEvent),
        sizeof(MthPeerStatus),
        sizeof(MthAbiLayout),
        sizeof(MthPeerEvent),
    };
    if (mth_abi_check_layout_v2(&layout, sizeof(layout)) != MTH_ERROR_SUCCESS) {
        return 4;
    }

    MthAbiLayout reported = {};
    if (mth_abi_layout_v2(&reported, sizeof(reported)) != MTH_ERROR_SUCCESS) {
        return 5;
    }
    if (reported.peer_event_size != sizeof(MthPeerEvent)) {
        return 6;
    }

    layout.peer_event_size += 1;
    if (mth_abi_check_layout_v2(&layout, sizeof(layout)) != MTH_ERROR_INVALID_ARGUMENT) {
        return 7;
    }
    if (mth_abi_layout_v2(nullptr, sizeof(MthAbiLayout)) != MTH_ERROR_NULL_POINTER) {
        return 8;
    }
    if (mth_abi_check_layout_v2(nullptr, sizeof(MthAbiLayout)) != MTH_ERROR_NULL_POINTER) {
        return 9;
    }

    struct GuardedLegacyLayout {
        LegacyMthAbiLayoutV1 layout;
        uint8_t canary[32];
    };
    GuardedLegacyLayout legacy = {
        {
            0u,
            1u,
            0u,
            sizeof(MthSessionConfig),
            sizeof(MthTableConfig),
            sizeof(MthBuffer),
            sizeof(MthEffect),
            sizeof(MthTableEvent),
            sizeof(MthPeerStatus),
            sizeof(LegacyMthAbiLayoutV1),
        },
        {},
    };
    for (uint8_t &byte : legacy.canary) {
        byte = 0xa5u;
    }
    const GuardedLegacyLayout original_legacy = legacy;

    if (mth_abi_layout(&legacy.layout) != MTH_ERROR_BUFFER_TOO_SMALL) {
        return 10;
    }
    if (std::memcmp(&legacy, &original_legacy, sizeof(legacy)) != 0) {
        return 11;
    }
    if (mth_abi_check_layout(legacy.layout) != MTH_ERROR_INVALID_ARGUMENT) {
        return 12;
    }
    if (mth_abi_layout_v2(
            reinterpret_cast<MthAbiLayout *>(&legacy.layout),
            sizeof(legacy.layout)) != MTH_ERROR_BUFFER_TOO_SMALL) {
        return 13;
    }
    if (std::memcmp(&legacy, &original_legacy, sizeof(legacy)) != 0) {
        return 14;
    }
    if (mth_abi_check_layout_v2(
            reinterpret_cast<const MthAbiLayout *>(&legacy.layout),
            sizeof(legacy.layout)) != MTH_ERROR_BUFFER_TOO_SMALL) {
        return 15;
    }
    if (std::memcmp(&legacy, &original_legacy, sizeof(legacy)) != 0) {
        return 16;
    }

    const uint8_t identity_seed[MTH_IDENTITY_SEED_BYTES] = {
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60,
        0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c, 0xc4,
        0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19,
        0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae, 0x7f, 0x60,
    };
    const uint8_t expected_public[MTH_IDENTITY_PUBLIC_KEY_BYTES] = {
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7,
        0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
        0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25,
        0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
    };
    uint8_t public_key[MTH_IDENTITY_PUBLIC_KEY_BYTES] = {};
    if (mth_identity_public_from_seed(identity_seed, public_key) != MTH_ERROR_SUCCESS) {
        return 17;
    }
    for (size_t index = 0; index < MTH_IDENTITY_PUBLIC_KEY_BYTES; ++index) {
        if (public_key[index] != expected_public[index]) {
            return 18;
        }
    }
    if (mth_identity_public_from_seed(nullptr, public_key) != MTH_ERROR_NULL_POINTER) {
        return 19;
    }
    if (mth_identity_public_from_seed(identity_seed, nullptr) != MTH_ERROR_NULL_POINTER) {
        return 20;
    }

    MthPeerEvent event = {};
    if (mth_peer_session_next_presentation_event(nullptr, &event) != MTH_ERROR_NULL_POINTER) {
        return 21;
    }
    mth_peer_event_free(event);
    return 0;
}
"#;
