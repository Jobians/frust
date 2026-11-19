//! End to end, the way an app author meets an out-of-tree plugin: a freshly
//! scaffolded app depends on `tests/fixtures/external-plugin` by path, and the
//! plugin is discovered from its own Cargo metadata, applied, and wired.
//!
//! No real cargo, Gradle or Xcode runs: `cargo metadata` is a canned answer
//! through a [`FakeProcessRunner`] (built from the fixture's real manifest), and
//! the shell crates the wiring step locates come from a [`StubLocator`]. All
//! output lands under the OS temp directory.

use frust_drive::packages::{CargoLocator, StubLocator};
use frust_drive::platform_wiring::{
    ANDROID_EMBEDDING_REL, ANDROID_SHELL_PACKAGE, IOS_EMBEDDING_REL, IOS_SHELL_PACKAGE,
    plugin_module_key, swift_package_link, sync_with_catalog,
};
use frust_drive::plugin::discover;
use frust_drive::plugin::{
    DepSource, PluginCatalog, PluginOrigin, pending_contributions, sync_plugins,
};
use frust_drive::process::{FakeProcessRunner, Output};
use frust_drive::scaffold::{FrustDependency, TemplateContext, generate};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "frust-drive-external-plugin-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-plugin")
}

fn test_context() -> TemplateContext {
    TemplateContext {
        project_name: "my_app".into(),
        title_case_name: "My App".into(),
        org: "dev.example".into(),
        description: "A new Frust application.".into(),
        frust_version: "0.1.0".into(),
        frust: FrustDependency::Path("/path/to/frust".into()),
        deeplink_scheme: None,
        deeplink_host: None,
    }
}

/// A scaffolded app whose manifest depends on the fixture by path.
fn app_with_fixture(tag: &str) -> PathBuf {
    let app = unique_temp_dir(tag);
    generate(&app, &test_context(), None, false, None).expect("scaffold generate");
    let manifest = app.join("Cargo.toml");
    let text = fs::read_to_string(&manifest).expect("read app Cargo.toml");
    let dep = format!(
        "[dependencies]\nfrust-ext-plugin-fixture = {{ path = {:?} }}",
        fixture_dir().to_string_lossy()
    );
    assert!(
        text.contains("[dependencies]"),
        "template has a [dependencies] table"
    );
    fs::write(&manifest, text.replacen("[dependencies]", &dep, 1)).expect("write app Cargo.toml");
    app
}

/// `cargo metadata` as cargo would answer it for `app`: the app, and the
/// fixture with its real `[package.metadata]`.
fn metadata_runner(app: &Path) -> FakeProcessRunner {
    let fixture = fixture_dir();
    let doc: toml::Table =
        toml::from_str(&fs::read_to_string(fixture.join("Cargo.toml")).unwrap()).unwrap();
    let json = serde_json::to_value(doc).unwrap();
    let metadata = serde_json::json!({
        "packages": [
            {
                "name": "my_app",
                "version": "0.1.0",
                "id": "my_app@0.1.0",
                "source": null,
                "manifest_path": app.join("Cargo.toml"),
                "features": {},
                "metadata": null,
                "dependencies": [],
            },
            {
                "name": "frust-ext-plugin-fixture",
                "version": "0.1.0",
                "id": "frust-ext-plugin-fixture@0.1.0",
                "source": null,
                "manifest_path": fixture.join("Cargo.toml"),
                "features": {},
                "metadata": json["package"]["metadata"],
                "dependencies": [],
            },
        ],
        "workspace_members": ["my_app@0.1.0"],
        // The app depends on the fixture directly, as its manifest says.
        "resolve": {
            "root": "my_app@0.1.0",
            "nodes": [
                {
                    "id": "my_app@0.1.0",
                    "dependencies": ["frust-ext-plugin-fixture@0.1.0"],
                    "deps": [{
                        "name": "frust_ext_plugin_fixture",
                        "pkg": "frust-ext-plugin-fixture@0.1.0",
                        "dep_kinds": [{ "kind": null, "target": null }],
                    }],
                },
                {
                    "id": "frust-ext-plugin-fixture@0.1.0",
                    "dependencies": [],
                    "deps": [],
                },
            ],
        },
        "version": 1,
    })
    .to_string();
    FakeProcessRunner::new().with(
        format!(
            "cargo metadata --format-version 1 --manifest-path {}",
            app.join("Cargo.toml").display()
        ),
        Output {
            success: true,
            stdout: metadata,
            stderr: String::new(),
        },
    )
}

/// Every file under `dir` (not following links), by relative path.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if kind.is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(rel, target.to_string_lossy().into_owned().into_bytes());
            } else if kind.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn an_out_of_tree_plugin_is_discovered_applied_and_wired() {
    let app = app_with_fixture("e2e");
    let runner = metadata_runner(&app);

    // Discovery: one path-sourced manifest plugin, nothing refused.
    let discovery = discover(&app, &CargoLocator::new(&runner)).expect("discover");
    assert!(discovery.problems.is_empty(), "{:?}", discovery.problems);
    assert_eq!(discovery.plugins.len(), 1);
    let entry = &discovery.plugins[0];
    assert_eq!(entry.spec.id, "ext-fixture");
    match &entry.origin {
        PluginOrigin::Manifest {
            package,
            package_dir,
            source,
            ..
        } => {
            assert_eq!(package, "frust-ext-plugin-fixture");
            assert_eq!(package_dir, &fixture_dir());
            assert_eq!(*source, DepSource::Path);
        }
        other => panic!("expected a manifest origin, got {other:?}"),
    }
    let (catalog, dropped) =
        PluginCatalog::with_discovered(PluginCatalog::built_in(), discovery.plugins);
    assert!(dropped.is_empty());
    let entry = catalog.find("ext-fixture").expect("fixture in catalog");

    // Every tracked item is pending: the dependency line is already there.
    let pending = pending_contributions(&app, entry, &[]).expect("pending");
    let labels: Vec<String> = pending.iter().map(|p| p.describe()).collect();
    let joined = labels.join("\n");
    for needle in [
        "frust-ext-fixture",
        "android.permission.NFC",
        "NSExtFixtureUsageDescription",
        "dev.example.extplugin.formats",
        "ExtPlugin",
        "CoreNFC",
        ".gitignore entry `/ios/ExtPlugin`",
    ] {
        assert!(
            joined.contains(needle),
            "`{needle}` not pending in:\n{joined}"
        );
    }
    assert_eq!(pending.len(), 7, "{joined}");

    // Sync applies them.
    let report = sync_plugins(&app, &catalog).expect("sync_plugins");
    assert!(report.changed());
    let read =
        |rel: &str| fs::read_to_string(app.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let settings = read("android/settings.gradle.kts");
    assert!(
        settings.contains("include(\":frust-ext-fixture\")"),
        "{settings}"
    );
    assert!(
        settings.contains(&plugin_module_key("frust-ext-fixture")),
        "{settings}"
    );
    let build = read("android/app/build.gradle.kts");
    assert!(
        build.contains("implementation(project(\":frust-ext-fixture\"))"),
        "{build}"
    );
    let pbxproj = read("ios/Runner.xcodeproj/project.pbxproj");
    assert!(
        pbxproj.contains("ExtPlugin"),
        "pbxproj lacks the package reference"
    );
    assert!(pbxproj.contains("CoreNFC"), "pbxproj lacks the framework");
    let plist = read("ios/Runner/Info.plist");
    assert!(
        plist.contains("<key>NSExtFixtureUsageDescription</key>"),
        "{plist}"
    );
    assert!(
        plist.contains("<string>Reads the fixture tag.</string>"),
        "{plist}"
    );
    assert!(
        plist.contains("<key>dev.example.extplugin.formats</key>"),
        "{plist}"
    );
    assert!(plist.contains("<string>NDEF</string>"), "{plist}");
    let ignore = read(".gitignore");
    assert_eq!(
        ignore.lines().filter(|l| *l == "/ios/ExtPlugin").count(),
        1,
        "{ignore}"
    );
    assert!(ignore.ends_with("/ios/ExtPlugin\n"), "{ignore}");
    let manifest = read("android/app/src/main/AndroidManifest.xml");
    assert!(manifest.contains("android.permission.NFC"), "{manifest}");

    assert!(
        pending_contributions(&app, entry, &[])
            .expect("pending")
            .is_empty()
    );

    // Machine-local wiring: shell crates come from a stub, the plugin's own
    // directories from its catalog entry.
    let shells = unique_temp_dir("shells");
    let android_shell = shells.join("android");
    let ios_shell = shells.join("ios");
    fs::create_dir_all(android_shell.join(ANDROID_EMBEDDING_REL)).unwrap();
    fs::create_dir_all(ios_shell.join(IOS_EMBEDDING_REL)).unwrap();
    let locator = StubLocator::new()
        .with(ANDROID_SHELL_PACKAGE, &android_shell)
        .with(IOS_SHELL_PACKAGE, &ios_shell);
    let wired = sync_with_catalog(&locator, &app, &catalog).expect("sync_with_catalog");
    assert!(wired.collisions.is_empty());
    assert_eq!(wired.plugins.len(), 2, "{:?}", wired.plugins);
    let properties = read("android/local.properties");
    let key = plugin_module_key("frust-ext-fixture");
    let line = properties
        .lines()
        .find(|l| l.starts_with(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no `{key}` in:\n{properties}"));
    assert!(line.ends_with("platform/android"), "{line}");
    let link = app.join(swift_package_link("ExtPlugin"));
    assert!(link.exists(), "ios link missing: {}", link.display());
    assert!(link.join("Package.swift").is_file());

    // A second sync changes nothing.
    let before = snapshot(&app);
    let again = sync_plugins(&app, &catalog).expect("second sync_plugins");
    assert!(!again.changed());
    let rewired = sync_with_catalog(&locator, &app, &catalog).expect("second sync_with_catalog");
    assert!(!rewired.changed());
    assert_eq!(snapshot(&app), before, "a second sync rewrote a file");

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&shells);
}

/// The fixture is what a published plugin looks like: its native directories
/// ride along in the package. `cargo package --list` needs no network for a
/// crate with no dependencies.
#[test]
fn the_packaged_crate_ships_its_native_directories() {
    // Packaged from a copy under the temp directory: cargo would otherwise look
    // upward for a workspace root, and what it finds depends on where the
    // checkout lives.
    let target = unique_temp_dir("package-target");
    let copy = unique_temp_dir("package-copy");
    copy_tree(&fixture_dir(), &copy);
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "package",
                "--list",
                "--allow-dirty",
                "--offline",
                "--manifest-path",
            ])
            .arg(copy.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .expect("run cargo package --list");
    let listed = String::from_utf8_lossy(&output.stdout).replace('\\', "/");
    assert!(
        output.status.success(),
        "cargo package --list failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for expected in [
        "platform/android/build.gradle.kts",
        "platform/android/consumer-rules.pro",
        "platform/android/src/main/AndroidManifest.xml",
        "platform/android/src/main/kotlin/dev/example/extplugin/ExtPluginHost.kt",
        "platform/ios/Package.swift",
        "platform/ios/Sources/ExtPlugin/ExtPlugin.swift",
    ] {
        assert!(
            listed.lines().any(|l| l == expected),
            "`{expected}` not packaged:\n{listed}"
        );
    }
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&copy);
}

/// A release build minifies the app, so the class the Rust side loads by name
/// survives only through consumer keep rules. The Kotlin source, the rules and
/// the Gradle reference are read from the fixture so the three cannot drift.
#[test]
fn the_fixture_keeps_its_jni_loaded_host_class_in_release_builds() {
    let android = fixture_dir().join("platform/android");
    let source =
        fs::read_to_string(android.join("src/main/kotlin/dev/example/extplugin/ExtPluginHost.kt"))
            .expect("read ExtPluginHost.kt");
    let declared = |prefix: &str| -> String {
        source
            .lines()
            .find_map(|l| l.trim().strip_prefix(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` line in:\n{source}"))
            .trim_end_matches(['{', ' '])
            .trim()
            .to_string()
    };
    let class = format!("{}.{}", declared("package "), declared("object "));

    let gradle = fs::read_to_string(android.join("build.gradle.kts")).expect("read gradle");
    assert!(
        gradle.contains("consumerProguardFiles(\"consumer-rules.pro\")"),
        "{gradle}"
    );
    let rules = fs::read_to_string(android.join("consumer-rules.pro"))
        .expect("the fixture ships platform/android/consumer-rules.pro");
    assert!(
        rules
            .lines()
            .any(|l| l.trim() == format!("-keep class {class} {{ *; }}")),
        "`{class}` is not kept by:\n{rules}"
    );
}
