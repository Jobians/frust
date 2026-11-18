//! `frust plugin add` against the real binary: help text, the failure paths
//! (which never reach cargo) and an end-to-end add on a registry-mode fixture
//! project whose plugin has no native module, so no package lookup runs and
//! no network is needed. `frust plugin list|sync` (and `add` of a plugin a
//! dependency describes in its own manifest) run against a path-only fixture
//! whose `cargo metadata` needs no network either; cargo is kept offline
//! throughout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn frust(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_frust"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("CARGO_NET_OFFLINE", "true")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .output()
        .expect("failed to spawn `frust`")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frust-plugin-add-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nfrust = { package = \"frust-ui\", version = \"0.6\" }\n",
    )
    .unwrap();
    dir
}

#[test]
fn help_shows_the_subcommand() {
    let dir = scratch("help");
    let top = frust(&dir, &["--help"]);
    assert!(text(&top.stdout).contains("plugin"));
    let sub = frust(&dir, &["plugin", "add", "--help"]);
    assert!(sub.status.success());
    let help = text(&sub.stdout);
    assert!(help.contains("--feature"), "{help}");
    assert!(help.contains("<ID>"), "{help}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adds_a_plugin_and_a_rerun_is_idempotent() {
    let dir = fixture("ok");
    let first = frust(&dir, &["plugin", "add", "device-info"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert!(text(&first.stdout).contains("[applied]"));
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("frust-device-info"));

    let second = frust(&dir, &["plugin", "add", "device-info"]);
    assert_eq!(second.status.code(), Some(0));
    assert!(!text(&second.stdout).contains("[applied]"));
    assert_eq!(
        std::fs::read_to_string(dir.join("Cargo.toml")).unwrap(),
        manifest
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_id_exits_1_and_lists_valid_ids() {
    let dir = fixture("badid");
    let out = frust(&dir, &["plugin", "add", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("unknown plugin `nope`"), "{err}");
    assert!(err.contains("device-info"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_feature_exits_1_and_lists_valid_features() {
    let dir = fixture("badfeat");
    let out = frust(
        &dir,
        &["plugin", "add", "network-image", "--feature", "nope"],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.contains("Valid features for `network-image`: webp"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outside_a_project_exits_1() {
    let dir = scratch("outside");
    let out = frust(&dir, &["plugin", "add", "device-info"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("Cargo.toml"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_shows_the_list_and_sync_subcommands() {
    let dir = scratch("help-list-sync");
    let plugin = frust(&dir, &["plugin", "--help"]);
    assert!(plugin.status.success());
    let help = text(&plugin.stdout);
    assert!(help.contains("list"), "{help}");
    assert!(help.contains("sync"), "{help}");
    for sub in ["list", "sync"] {
        let out = frust(&dir, &["plugin", sub, "--help"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An Android-only app (no `ios/`) at `<root>/app` with a path dependency on
/// `<root>/acme-ble`, a package describing itself as a plugin with an
/// Android permission and, `with_ios`, an iOS plist key. Both are real Cargo
/// packages, so `cargo metadata` resolves the graph offline.
fn manifest_plugin_fixture(tag: &str, with_ios: bool) -> PathBuf {
    let root = scratch(tag);
    let ios = if with_ios {
        "\n[package.metadata.frust.plugin.ios]\n\
         plist = { NSBluetoothAlwaysUsageDescription = \"Finds nearby Acme devices.\" }\n"
    } else {
        ""
    };
    let plugin = root.join("acme-ble");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("Cargo.toml"),
        format!(
            "[package]\nname = \"frust-acme-ble\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\npath = \"lib.rs\"\n\n\
         [package.metadata.frust.plugin]\nid = \"acme-ble\"\nsummary = \"Acme BLE scanner\"\n\n\
         [package.metadata.frust.plugin.android]\n\
         permissions = [\"android.permission.BLUETOOTH_CONNECT\"]\n{ios}",
        ),
    )
    .unwrap();
    std::fs::write(plugin.join("lib.rs"), "").unwrap();
    let app = root.join("app");
    std::fs::create_dir_all(app.join("android/app/src/main")).unwrap();
    std::fs::write(
        app.join("Cargo.toml"),
        "[package]\nname = \"acme_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\npath = \"lib.rs\"\n\n\
         [dependencies]\nfrust-acme-ble = { path = \"../acme-ble\" }\n",
    )
    .unwrap();
    std::fs::write(app.join("lib.rs"), "").unwrap();
    std::fs::write(
        app.join(MANIFEST),
        "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n</manifest>\n",
    )
    .unwrap();
    root
}

const MANIFEST: &str = "android/app/src/main/AndroidManifest.xml";

fn acme_line(listing: &str) -> String {
    listing
        .lines()
        .find(|line| line.starts_with("acme-ble "))
        .unwrap_or_else(|| panic!("no acme-ble line in:\n{listing}"))
        .to_string()
}

#[test]
fn list_and_sync_a_plugin_a_dependency_describes() {
    let root = manifest_plugin_fixture("list-sync", true);
    let app = root.join("app");

    let list = frust(&app, &["plugin", "list"]);
    assert_eq!(list.status.code(), Some(0), "{}", text(&list.stderr));
    let listing = text(&list.stdout);
    let acme = acme_line(&listing);
    assert!(acme.contains(" frust-acme-ble 0.1.0 (path) "), "{acme}");
    assert!(
        acme.ends_with(" 1 pending"),
        "the iOS key is skipped: {acme}"
    );
    assert!(listing.lines().any(|line| line.starts_with("device-info ")));

    let sync = frust(&app, &["plugin", "sync"]);
    assert_eq!(sync.status.code(), Some(0), "{}", text(&sync.stderr));
    assert_eq!(
        text(&sync.stdout),
        "Plugin `acme-ble`: 1 applied\n  \
         [applied] AndroidManifest.xml permission `android.permission.BLUETOOTH_CONNECT`\n"
    );
    let manifest = std::fs::read_to_string(app.join(MANIFEST)).unwrap();
    assert!(
        manifest.contains("android.permission.BLUETOOTH_CONNECT"),
        "{manifest}"
    );
    assert!(!app.join("ios").exists());

    let again = frust(&app, &["plugin", "sync"]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
    assert_eq!(text(&again.stdout), "Plugin `acme-ble`: up to date\n");
    assert_eq!(
        std::fs::read_to_string(app.join(MANIFEST)).unwrap(),
        manifest
    );

    let list = frust(&app, &["plugin", "list"]);
    assert!(acme_line(&text(&list.stdout)).ends_with(" added"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn adds_a_plugin_a_dependency_describes_and_a_rerun_is_idempotent() {
    let root = manifest_plugin_fixture("add-discovered", false);
    let app = root.join("app");
    let first = frust(&app, &["plugin", "add", "acme-ble"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert!(text(&first.stdout).contains("[applied]"));
    let manifest = std::fs::read_to_string(app.join(MANIFEST)).unwrap();

    let second = frust(&app, &["plugin", "add", "acme-ble"]);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    assert!(!text(&second.stdout).contains("[applied]"));
    assert_eq!(
        std::fs::read_to_string(app.join(MANIFEST)).unwrap(),
        manifest
    );

    let unknown = frust(&app, &["plugin", "add", "nope"]);
    assert_eq!(unknown.status.code(), Some(1));
    let err = text(&unknown.stderr);
    assert!(
        err.contains("device-info") && err.contains("acme-ble"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_outside_a_project_exits_1() {
    let dir = scratch("sync-outside");
    let out = frust(&dir, &["plugin", "sync"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("Cargo.toml"));
    let _ = std::fs::remove_dir_all(&dir);
}
