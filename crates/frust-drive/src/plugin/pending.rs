//! Which of a plugin's tracked contributions an app still lacks
//! ([`pending_contributions`]), applying every manifest-described plugin the
//! app depends on ([`sync_plugins`]), and the warning the machine-local
//! wiring returns when two plugins claim one native module name
//! ([`ModuleCollision`]). Every "already present" answer here is the very
//! predicate the matching applier in `apply.rs` skips by, so the two cannot
//! drift. Reads only; [`sync_plugins`] writes through the appliers.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

use super::apply::{self, DepLookup};
use super::catalog::{CatalogEntry, PluginCatalog};
use super::{AddOutcome, AddReport, Contribution, NativeKind, PluginAddError};
use crate::packages::{PackageLocator, PackagesError};

/// One tracked contribution of a plugin that the app does not carry yet —
/// one [`pending_contributions`] row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingContribution {
    /// The owning plugin's id.
    pub plugin_id: Cow<'static, str>,
    /// The missing contribution, as the catalog states it.
    pub contribution: Contribution,
}

impl PendingContribution {
    /// The contribution's report label ([`Contribution::describe`]).
    pub fn describe(&self) -> String {
        self.contribution.describe()
    }
}

/// The tracked contributions of `entry` — its base and the requested
/// `features`' — that the project at `project_root` does not carry yet, in
/// report order, computed without writing anything. Each is judged by the
/// predicate its applier skips by: the dependency line, the Cargo feature,
/// the `uses-permission`, the `Info.plist` `<key>`, both Gradle include
/// lines, the repository url, all six pbxproj package sites, the framework
/// in every `OTHER_LDFLAGS` list, and an entitlement's key in every file the
/// Runner configurations sign with plus `CODE_SIGN_ENTITLEMENTS` in each of
/// them. Machine-local wiring (`local.properties` keys, `ios/` links) and
/// the desktop lane, applied by every build, are never pending. A project
/// file a contribution needs that is missing or unreadable fails exactly as
/// adding the plugin would — except that a manifest-described plugin's
/// contributions for a platform whose directory (`android/`, `ios/`) the
/// project lacks are skipped, never pending, the way the machine-local
/// wiring skips that platform.
pub fn pending_contributions(
    project_root: &Path,
    entry: &CatalogEntry,
    features: &[&str],
) -> Result<Vec<PendingContribution>, PluginAddError> {
    let entry = &*for_project(project_root, entry);
    let contributions = apply::selected_contributions(&entry.spec, features)?;
    let doc = apply::read_cargo_doc(project_root)?;
    let mut pending = Vec::new();
    for contribution in contributions {
        if is_pending(project_root, entry, &doc, contribution)? {
            pending.push(PendingContribution {
                plugin_id: entry.spec.id.clone(),
                contribution: contribution.clone(),
            });
        }
    }
    Ok(pending)
}

/// The platform directory a contribution edits a file under, if it edits
/// one: `android` or `ios`.
fn platform_dir(contribution: &Contribution) -> Option<&'static str> {
    match contribution {
        Contribution::ManifestPermission { .. }
        | Contribution::GradleModule { .. }
        | Contribution::MavenRepository { .. } => Some("android"),
        Contribution::PlistEntry { .. }
        | Contribution::PlistArrayEntry { .. }
        | Contribution::SwiftPackageRef { .. }
        | Contribution::IosFramework { .. }
        | Contribution::IosEntitlement { .. } => Some("ios"),
        _ => None,
    }
}

/// `entry` as it applies to the project at `project_root`: a built-in entry
/// unchanged, a manifest-described one without its contributions (base and
/// features alike) for a platform whose directory the project lacks.
fn for_project<'e>(project_root: &Path, entry: &'e CatalogEntry) -> Cow<'e, CatalogEntry> {
    let absent = |contribution: &Contribution| {
        platform_dir(contribution).is_some_and(|dir| !project_root.join(dir).is_dir())
    };
    let mut all = entry.spec.base.iter().chain(
        entry
            .spec
            .optional_features
            .iter()
            .flat_map(|f| f.contributions.iter()),
    );
    if entry.is_built_in() || !all.any(&absent) {
        return Cow::Borrowed(entry);
    }
    let keep = |contributions: &[Contribution]| -> Vec<Contribution> {
        contributions
            .iter()
            .filter(|c| !absent(c))
            .cloned()
            .collect()
    };
    let mut entry = entry.clone();
    entry.spec.base = Cow::Owned(keep(&entry.spec.base));
    let features = entry
        .spec
        .optional_features
        .iter()
        .map(|feature| {
            let mut feature = feature.clone();
            feature.contributions = Cow::Owned(keep(&feature.contributions));
            feature
        })
        .collect();
    entry.spec.optional_features = Cow::Owned(features);
    Cow::Owned(entry)
}

/// The text of project file `rel`, failing as its applier would.
fn read(project_root: &Path, rel: &str) -> Result<String, PluginAddError> {
    apply::read_required(&project_root.join(rel), rel)
}

/// Whether `contribution` of `entry` still has to be applied.
fn is_pending(
    project_root: &Path,
    entry: &CatalogEntry,
    doc: &DocumentMut,
    contribution: &Contribution,
) -> Result<bool, PluginAddError> {
    let root = project_root;
    let present = match contribution {
        Contribution::CargoDep { name } => apply::cargo_dep_present(doc, name, &entry.origin),
        Contribution::CargoFeature { name, feature } => {
            apply::cargo_feature_present(doc, name, feature, DepLookup::of(&entry.origin))
        }
        Contribution::ManifestPermission { permission } => {
            apply::manifest_permission_present(&read(root, apply::MANIFEST_REL)?, permission)
        }
        Contribution::PlistEntry { key, .. } | Contribution::PlistArrayEntry { key, .. } => {
            apply::plist_key_present(&read(root, apply::PLIST_REL)?, key)
        }
        Contribution::GradleModule { gradle_name, .. } => {
            let settings = read(root, apply::SETTINGS_GRADLE_REL)?;
            let build = read(root, apply::APP_BUILD_GRADLE_REL)?;
            apply::settings_include_present(&settings, gradle_name)
                && apply::app_dependency_present(&build, gradle_name)
        }
        Contribution::MavenRepository { url, .. } => {
            apply::maven_repository_present(&read(root, apply::SETTINGS_GRADLE_REL)?, url)
        }
        Contribution::SwiftPackageRef { package_name, .. } => {
            apply::swift_package_present(&read(root, apply::PBXPROJ_REL)?, package_name)?
        }
        Contribution::IosFramework { name } => {
            apply::framework_linked(&read(root, apply::PBXPROJ_REL)?, name)?
        }
        Contribution::IosEntitlement {
            key,
            value,
            comment,
        } => apply::ios_entitlement_present(root, key, value, comment)?,
        Contribution::AppCrateMacro { invocation, .. } => {
            apply::app_crate_macro_present(&read(root, apply::LIB_RS_REL)?, invocation)
        }
        Contribution::ScaffoldFile { rel_path, .. } => {
            apply::scaffold_file_present(root, rel_path)?
        }
        Contribution::MacosPlistEntry { .. }
        | Contribution::MacosEntitlement { .. }
        | Contribution::LinuxDesktopEntry { .. } => true,
    };
    Ok(!present)
}

/// Refuse a pending contribution whose insertion anchor its target file
/// lacks — the check [`sync_plugins`] makes for every plugin before its
/// first write, since an applier only finds a missing anchor once earlier
/// plugins have been written.
fn check_anchors(
    project_root: &Path,
    pending: &[PendingContribution],
) -> Result<(), PluginAddError> {
    let require = |rel: &str, anchor: &str| -> Result<(), PluginAddError> {
        if read(project_root, rel)?.contains(anchor) {
            Ok(())
        } else {
            Err(PluginAddError::MalformedProjectFile(rel.to_string()))
        }
    };
    for row in pending {
        match &row.contribution {
            Contribution::ManifestPermission { .. } => {
                require(apply::MANIFEST_REL, apply::MANIFEST_ANCHOR)?;
            }
            Contribution::PlistEntry { .. } | Contribution::PlistArrayEntry { .. } => {
                require(apply::PLIST_REL, apply::PLIST_ANCHOR)?;
            }
            Contribution::GradleModule { gradle_name, .. } => {
                let settings = read(project_root, apply::SETTINGS_GRADLE_REL)?;
                if !apply::settings_include_present(&settings, gradle_name) {
                    require(apply::SETTINGS_GRADLE_REL, apply::SETTINGS_ANCHOR)?;
                }
                let build = read(project_root, apply::APP_BUILD_GRADLE_REL)?;
                if !apply::app_dependency_present(&build, gradle_name) {
                    require(apply::APP_BUILD_GRADLE_REL, apply::APP_DEPS_ANCHOR)?;
                }
            }
            Contribution::MavenRepository { .. } => {
                require(apply::SETTINGS_GRADLE_REL, apply::REPOSITORIES_ANCHOR)?;
            }
            // The pbxproj sites, `OTHER_LDFLAGS` lists and Runner
            // configurations were resolved by the pending check itself.
            _ => {}
        }
    }
    Ok(())
}

/// What [`sync_plugins`] did: one [`AddReport`] per manifest-described
/// plugin the app depends on, in catalog order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Per plugin, every base contribution applied or already present.
    pub plugins: Vec<AddReport>,
}

impl SyncReport {
    /// Whether any plugin's contribution was applied this run.
    pub fn changed(&self) -> bool {
        self.plugins
            .iter()
            .flat_map(|report| &report.items)
            .any(|item| item.outcome == AddOutcome::Applied)
    }
}

/// Apply the base contributions of every manifest-described plugin in
/// `catalog` that the project at `project_root` depends on (a non-optional
/// entry on its package in `[dependencies]` or a target table) — optional
/// features are chosen with an add, never here. Built-in plugins are left
/// to `add_plugin`. Every plugin is checked before the first write — its
/// platform floors, native paths, entitlement plan, Maven marker and the
/// insertion anchor of each pending contribution — so a refusal leaves the
/// project untouched. Idempotent like `add_plugin`: a second run reports
/// every item [`AddOutcome::AlreadyPresent`]. A contribution for a platform
/// whose directory the project lacks is skipped, as in
/// [`pending_contributions`]: neither applied nor reported.
pub fn sync_plugins(
    project_root: &Path,
    catalog: &PluginCatalog,
) -> Result<SyncReport, PluginAddError> {
    let doc = apply::read_cargo_doc(project_root)?;
    let entries: Vec<Cow<'_, CatalogEntry>> = catalog
        .iter()
        .filter(|entry| !entry.is_built_in() && apply::entry_is_installed(entry, &doc))
        .map(|entry| for_project(project_root, entry))
        .collect();
    for entry in &entries {
        apply::prepare_add(project_root, entry, &[])?;
        check_anchors(
            project_root,
            &pending_contributions(project_root, entry, &[])?,
        )?;
    }
    let mut plugins = Vec::with_capacity(entries.len());
    for entry in &entries {
        plugins.push(apply::add_entry_with(
            &CatalogLocated,
            project_root,
            entry,
            &[],
        )?);
    }
    Ok(SyncReport { plugins })
}

/// The locator a manifest-described plugin's add is given: its package
/// directory comes from its catalog entry, so cargo is never asked.
struct CatalogLocated;

impl PackageLocator for CatalogLocated {
    fn locate_many(
        &self,
        project_dir: &Path,
        packages: &[&str],
    ) -> Result<Vec<PathBuf>, PackagesError> {
        Err(PackagesError::MetadataUnreadable {
            project_dir: project_dir.to_path_buf(),
            reason: format!(
                "{packages:?}: a manifest-described plugin is located by its catalog entry"
            ),
        })
    }
}

/// Two installed plugins declare the same Gradle module or Swift package
/// name; the machine-local wiring can point the name at only one of them.
/// A built-in plugin wins, otherwise the first by package name; the other's
/// module is left unwired. Returned by `platform_wiring::sync_with_catalog`
/// in its report, never printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleCollision {
    /// The module, with the name both declare.
    pub kind: NativeKind,
    /// The plugin whose module is wired.
    pub kept_plugin_id: String,
    /// The plugin whose module is not wired.
    pub dropped_plugin_id: String,
}

impl fmt::Display for ModuleCollision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let module = match &self.kind {
            NativeKind::GradleModule { gradle_name } => format!("Android module `{gradle_name}`"),
            NativeKind::SwiftPackage { package_name } => format!("iOS package `{package_name}`"),
        };
        write!(
            f,
            "{module}: declared by plugins `{}` and `{}`; `{}`'s is wired, `{}`'s is not",
            self.kept_plugin_id,
            self.dropped_plugin_id,
            self.kept_plugin_id,
            self.dropped_plugin_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_build::local_properties;
    use crate::packages::{GraphPackage, StubLocator};
    use crate::platform_wiring;
    use crate::plugin::manifest;
    use crate::scaffold::{self, TemplateContext};
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_root(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "frust-drive-plugin-pending-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        crate::host_path::canonicalize_simplified(&dir).unwrap()
    }

    /// A freshly scaffolded app at `<root>/app`.
    fn scaffold_app(root: &Path) -> PathBuf {
        let app = root.join("app");
        let ctx = TemplateContext {
            project_name: "acme_app".into(),
            title_case_name: "Acme App".into(),
            org: "com.example".into(),
            description: "A test app.".into(),
            frust_version: "0.1.0".into(),
            frust: scaffold::FrustDependency::Registry {
                version: "0.1.0".into(),
            },
            deeplink_scheme: None,
            deeplink_host: None,
        };
        scaffold::generate(&app, &ctx, None, false, None).unwrap();
        app
    }

    const PACKAGE: &str = "frust-acme-ble";

    /// The plugin's own `Cargo.toml` metadata: a Gradle module, a
    /// permission, a Swift package and a plist key.
    const MANIFEST: &str = r#"
        [package.metadata.frust.plugin]
        id = "acme-ble"
        summary = "Acme BLE scanner"

        [package.metadata.frust.plugin.android]
        gradle-module = { name = "frust-acme-ble", path = "platform/android" }
        permissions = ["android.permission.BLUETOOTH_CONNECT"]

        [package.metadata.frust.plugin.ios]
        swift-package = { name = "FrustAcmeBle", path = "platform/ios" }
        plist = { NSBluetoothAlwaysUsageDescription = "Finds nearby Acme devices." }
    "#;

    /// A plugin package at `<root>/<dir>` described by `metadata`, parsed
    /// by discovery's own parser into a catalog entry.
    fn plugin_entry(
        root: &Path,
        dir: &str,
        package: &str,
        source: Option<&str>,
        metadata: &str,
    ) -> CatalogEntry {
        let package_dir = root.join(dir);
        fs::create_dir_all(package_dir.join("platform/android")).unwrap();
        fs::create_dir_all(package_dir.join("platform/ios")).unwrap();
        let doc: toml::Table = toml::from_str(metadata).unwrap();
        let graph_package = GraphPackage {
            name: package.to_string(),
            version: "0.3.1".to_string(),
            source: source.map(str::to_string),
            manifest_path: package_dir.join("Cargo.toml"),
            metadata: serde_json::to_value(doc).unwrap()["package"]["metadata"].clone(),
            features: None,
        };
        manifest::plugin_entry(&graph_package, &graph_package.metadata["frust"]["plugin"]).unwrap()
    }

    /// `line` (`name = value`) added to the app's `[dependencies]`.
    fn depend(app: &Path, line: &str) {
        let path = app.join("Cargo.toml");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replacen("[dependencies]\n", &format!("[dependencies]\n{line}\n"), 1),
        )
        .unwrap();
    }

    /// Every file under `root` (symlinks recorded by target, never followed).
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.file_type().is_symlink() {
                    let target = fs::read_link(&path).unwrap();
                    out.insert(path, target.to_string_lossy().as_bytes().to_vec());
                } else if meta.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, &mut out);
        out
    }

    fn read(app: &Path, rel: &str) -> String {
        fs::read_to_string(app.join(rel)).unwrap()
    }

    #[test]
    fn a_manifest_plugin_applies_from_its_own_package_and_is_then_not_pending() {
        let root = temp_root("apply");
        let app = scaffold_app(&root);
        let entry = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        let line = "frust-acme-ble = { path = \"../acme-ble\", default-features = false }";
        depend(&app, line);
        let cargo_before = read(&app, "Cargo.toml");

        // Before: every contribution but the dependency line is pending.
        let before = pending_contributions(&app, &entry, &[]).unwrap();
        let expected: Vec<&Contribution> = entry.spec.base.iter().skip(1).collect();
        assert_eq!(
            before.iter().map(|p| &p.contribution).collect::<Vec<_>>(),
            expected
        );
        assert!(before.iter().all(|p| p.plugin_id == "acme-ble"));

        let catalog =
            PluginCatalog::with_discovered(PluginCatalog::built_in(), vec![entry.clone()]).0;
        let locator = StubLocator::failing("a manifest plugin is never located");
        let report =
            apply::add_plugin_from_with(&locator, &app, &catalog, "acme-ble", &[]).unwrap();
        assert_eq!(locator.calls(), 0);
        let outcomes: Vec<AddOutcome> = report.items.iter().map(|i| i.outcome).collect();
        assert_eq!(
            outcomes,
            [
                AddOutcome::AlreadyPresent,
                AddOutcome::Applied,
                AddOutcome::Applied,
                AddOutcome::Applied,
                AddOutcome::Applied,
            ]
        );
        assert_eq!(
            read(&app, "Cargo.toml"),
            cargo_before,
            "the dependency line is the app's"
        );
        assert!(read(&app, apply::SETTINGS_GRADLE_REL).contains("include(\":frust-acme-ble\")"));
        assert!(
            read(&app, apply::APP_BUILD_GRADLE_REL)
                .contains("implementation(project(\":frust-acme-ble\"))")
        );
        assert!(
            read(&app, apply::MANIFEST_REL)
                .contains("android:name=\"android.permission.BLUETOOTH_CONNECT\"")
        );
        assert!(
            read(&app, apply::PLIST_REL).contains("<key>NSBluetoothAlwaysUsageDescription</key>")
        );
        assert!(
            read(&app, apply::PBXPROJ_REL)
                .contains("/* XCLocalSwiftPackageReference \"FrustAcmeBle\" */ = {")
        );
        // The machine-local wiring points into the package itself.
        let android_dir = root.join("acme-ble/platform/android");
        assert_eq!(
            local_properties::read_value(
                &app.join(platform_wiring::LOCAL_PROPERTIES),
                "frust.plugin.frust-acme-ble.dir"
            )
            .unwrap(),
            Some(android_dir.display().to_string())
        );
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(app.join("ios/FrustAcmeBle")).unwrap(),
            root.join("acme-ble/platform/ios")
        );

        // A second add is a byte-identical no-op, and nothing is pending.
        let tree = snapshot(&app);
        let again = apply::add_plugin_from_with(&locator, &app, &catalog, "acme-ble", &[]).unwrap();
        assert!(
            again
                .items
                .iter()
                .all(|i| i.outcome == AddOutcome::AlreadyPresent),
            "{again:?}"
        );
        assert_eq!(snapshot(&app), tree);
        assert_eq!(pending_contributions(&app, &entry, &[]).unwrap(), []);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_dependency_line_is_written_from_the_package_source() {
        let root = temp_root("dep-line");
        let path_entry = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        let registry = Some("registry+https://github.com/rust-lang/crates.io-index");
        let registry_entry = plugin_entry(&root, "registry/acme-ble", PACKAGE, registry, MANIFEST);
        for (entry, expected) in [
            (&path_entry, "frust-acme-ble = { path = \"../acme-ble\" }\n"),
            (&registry_entry, "frust-acme-ble = \"0.3.1\"\n"),
        ] {
            let app = scaffold_app(&root);
            let report = apply::add_entry_with(&StubLocator::new(), &app, entry, &[]).unwrap();
            assert_eq!(report.items[0].outcome, AddOutcome::Applied);
            let cargo = read(&app, "Cargo.toml");
            assert!(cargo.contains(expected), "{cargo}");
            assert!(
                !cargo.contains("plugins/acme-ble"),
                "never derived from the frust path: {cargo}"
            );
            let _ = fs::remove_dir_all(&app);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_git_package_without_a_dependency_line_is_refused_untouched() {
        let root = temp_root("git");
        let app = scaffold_app(&root);
        let git = Some("git+https://example.com/acme/frust-acme-ble#0123abcd");
        let entry = plugin_entry(&root, "acme-ble", PACKAGE, git, MANIFEST);
        let tree = snapshot(&app);
        let err = apply::add_entry_with(&StubLocator::new(), &app, &entry, &[]).unwrap_err();
        assert!(
            matches!(&err, PluginAddError::DependencySourceNotWritable { package, .. } if package == PACKAGE),
            "{err:?}"
        );
        assert!(err.to_string().contains("yourself"), "{err}");
        assert_eq!(snapshot(&app), tree);

        // The author's own git line is the app's to keep.
        depend(
            &app,
            "frust-acme-ble = { git = \"https://example.com/acme/frust-acme-ble\" }",
        );
        let report = apply::add_entry_with(&StubLocator::new(), &app, &entry, &[]).unwrap();
        assert_eq!(report.items[0].outcome, AddOutcome::AlreadyPresent);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manifest_plugin_never_runs_app_crate_or_scaffold_contributions() {
        let root = temp_root("inexpressible");
        let app = scaffold_app(&root);
        let mut entry = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        let mut base = entry.spec.base.to_vec();
        base.push(Contribution::ScaffoldFile {
            rel_path: Cow::Borrowed("acme.txt"),
            contents: Cow::Borrowed(""),
            comment: Cow::Borrowed("test"),
        });
        entry.spec.base = Cow::Owned(base);
        let tree = snapshot(&app);
        let err = apply::add_entry_with(&StubLocator::new(), &app, &entry, &[]).unwrap_err();
        assert!(
            matches!(err, PluginAddError::InvalidContribution(_)),
            "{err:?}"
        );
        assert_eq!(snapshot(&app), tree);

        // Nor can the schema express either: discovery never produces one.
        let parsed = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        assert!(!parsed.spec.base.iter().any(|c| matches!(
            c,
            Contribution::AppCrateMacro { .. } | Contribution::ScaffoldFile { .. }
        )));
        let _ = fs::remove_dir_all(&root);
    }

    const ENTITLEMENT_MANIFEST: &str = r#"
        [package.metadata.frust.plugin]
        id = "acme-push"
        summary = "Acme push"
        [package.metadata.frust.plugin.ios]
        entitlements = { "aps-environment" = "development" }
    "#;

    /// A string entitlement is pending until its key is in the file, and
    /// again as soon as any one Runner configuration lacks
    /// `CODE_SIGN_ENTITLEMENTS`.
    #[test]
    fn an_entitlement_is_pending_while_any_runner_configuration_lacks_the_setting() {
        let root = temp_root("entitlement");
        let app = scaffold_app(&root);
        let entry = plugin_entry(
            &root,
            "acme-push",
            "frust-acme-push",
            None,
            ENTITLEMENT_MANIFEST,
        );
        depend(&app, "frust-acme-push = { path = \"../acme-push\" }");
        let entitlement = &entry.spec.base[1];
        assert!(matches!(entitlement, Contribution::IosEntitlement { .. }));

        let before = pending_contributions(&app, &entry, &[]).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(&before[0].contribution, entitlement);

        apply::add_entry_with(&StubLocator::new(), &app, &entry, &[]).unwrap();
        assert_eq!(pending_contributions(&app, &entry, &[]).unwrap(), []);
        let setting = "CODE_SIGN_ENTITLEMENTS = Runner/Runner.entitlements;";
        let pbxproj = read(&app, apply::PBXPROJ_REL);
        assert!(pbxproj.matches(setting).count() >= 3, "{pbxproj}");

        fs::write(
            app.join(apply::PBXPROJ_REL),
            pbxproj.replacen(setting, "", 1),
        )
        .unwrap();
        let after = pending_contributions(&app, &entry, &[]).unwrap();
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after[0].describe(), entitlement.describe());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pending_refuses_an_unknown_feature_and_never_lists_the_desktop_lane() {
        let root = temp_root("features");
        let app = scaffold_app(&root);
        let mut entry = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        assert!(matches!(
            pending_contributions(&app, &entry, &["nope"]),
            Err(PluginAddError::UnknownFeature { .. })
        ));
        entry.spec.base = Cow::Owned(vec![
            Contribution::CargoDep {
                name: Cow::Borrowed(PACKAGE),
            },
            Contribution::MacosEntitlement {
                key: Cow::Borrowed("com.apple.security.network.client"),
                comment: Cow::Borrowed("test"),
            },
        ]);
        let pending = pending_contributions(&app, &entry, &[]).unwrap();
        assert_eq!(pending.len(), 1, "only the dependency line: {pending:?}");
        let _ = fs::remove_dir_all(&root);
    }

    /// A catalog of the built-in registry plus `discovered`.
    fn catalog(discovered: Vec<CatalogEntry>) -> PluginCatalog {
        PluginCatalog::with_discovered(PluginCatalog::built_in(), discovered).0
    }

    #[test]
    fn sync_applies_every_installed_manifest_plugin_once() {
        let root = temp_root("sync");
        let app = scaffold_app(&root);
        let ble = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        let push = plugin_entry(
            &root,
            "acme-push",
            "frust-acme-push",
            None,
            ENTITLEMENT_MANIFEST,
        );
        // Only `acme-ble` is a dependency of the app, in a target table.
        let path = app.join("Cargo.toml");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            format!(
                "{text}\n[target.'cfg(target_os = \"android\")'.dependencies]\n\
                 frust-acme-ble = {{ path = \"../acme-ble\" }}\n"
            ),
        )
        .unwrap();
        let catalog = catalog(vec![ble.clone(), push]);

        let report = sync_plugins(&app, &catalog).unwrap();
        assert_eq!(report.plugins.len(), 1, "{report:?}");
        assert_eq!(report.plugins[0].plugin_id, "acme-ble");
        assert!(report.changed());
        assert_eq!(pending_contributions(&app, &ble, &[]).unwrap(), []);

        let tree = snapshot(&app);
        let again = sync_plugins(&app, &catalog).unwrap();
        assert!(!again.changed(), "{again:?}");
        assert_eq!(snapshot(&app), tree);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_checks_every_plugin_before_the_first_write() {
        let root = temp_root("sync-refused");
        let app = scaffold_app(&root);
        let ble = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        let high_floor = MANIFEST.replace("acme-ble\"\n", "acme-tag\"\n").replace(
            "summary = \"Acme BLE scanner\"",
            "summary = \"Acme tags\"\nmin-ios = \"99\"",
        );
        let tag = plugin_entry(&root, "acme-tag", "frust-acme-tag", None, &high_floor);
        assert_eq!(tag.spec.min_ios.as_deref(), Some("99"));
        depend(&app, "frust-acme-ble = { path = \"../acme-ble\" }");
        depend(&app, "frust-acme-tag = { path = \"../acme-tag\" }");
        let tree = snapshot(&app);

        let err = sync_plugins(&app, &catalog(vec![ble, tag])).unwrap_err();
        assert!(
            matches!(&err, PluginAddError::PlatformFloorTooLow { plugin, .. } if &**plugin == "acme-tag"),
            "{err:?}"
        );
        assert_eq!(snapshot(&app), tree, "nothing was written");

        // A pending contribution whose anchor is gone is refused up front too.
        let manifest = read(&app, apply::MANIFEST_REL).replace("</manifest>", "");
        fs::write(app.join(apply::MANIFEST_REL), manifest).unwrap();
        let tree = snapshot(&app);
        let ble = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
        let err = sync_plugins(&app, &catalog(vec![ble])).unwrap_err();
        assert!(
            matches!(&err, PluginAddError::MalformedProjectFile(rel) if rel == apply::MANIFEST_REL),
            "{err:?}"
        );
        assert_eq!(snapshot(&app), tree);
        let _ = fs::remove_dir_all(&root);
    }

    /// An app without `ios/` (or without `android/`) syncs a manifest plugin
    /// that also carries items for that platform: they are never pending,
    /// never applied, and the platform directory is not created.
    #[test]
    fn a_manifest_plugin_skips_a_platform_directory_the_app_lacks() {
        for (absent, kept) in [("ios", "android"), ("android", "ios")] {
            let root = temp_root(&format!("no-{absent}"));
            let app = scaffold_app(&root);
            fs::remove_dir_all(app.join(absent)).unwrap();
            let entry = plugin_entry(&root, "acme-ble", PACKAGE, None, MANIFEST);
            depend(&app, "frust-acme-ble = { path = \"../acme-ble\" }");

            let pending = pending_contributions(&app, &entry, &[]).unwrap();
            assert_eq!(pending.len(), 2, "{absent}: {pending:?}");
            assert!(
                pending
                    .iter()
                    .all(|p| platform_dir(&p.contribution) == Some(kept)),
                "{absent}: {pending:?}"
            );

            let catalog = catalog(vec![entry.clone()]);
            let report = sync_plugins(&app, &catalog).unwrap();
            assert_eq!(report.plugins.len(), 1, "{report:?}");
            assert_eq!(
                report.plugins[0].items.len(),
                3,
                "the dependency line and the {kept} items only: {report:?}"
            );
            assert!(report.changed());
            assert!(!app.join(absent).exists(), "{absent}/ is never created");
            assert_eq!(pending_contributions(&app, &entry, &[]).unwrap(), []);

            let tree = snapshot(&app);
            let again = sync_plugins(&app, &catalog).unwrap();
            assert!(!again.changed(), "{again:?}");
            assert_eq!(snapshot(&app), tree);
            let _ = fs::remove_dir_all(&root);
        }
    }

    /// The skip is for manifest-described plugins only: a built-in plugin's
    /// iOS item on an app without `ios/` still fails as its add would.
    #[test]
    fn a_built_in_plugin_still_needs_the_platform_files_it_edits() {
        let root = temp_root("built-in-no-ios");
        let app = scaffold_app(&root);
        fs::remove_dir_all(app.join("ios")).unwrap();
        let entry = CatalogEntry::built_in(crate::plugin::PluginSpec {
            id: Cow::Borrowed("acme-tag"),
            crate_dir: Cow::Borrowed("acme-tag"),
            base: Cow::Owned(vec![
                Contribution::CargoDep {
                    name: Cow::Borrowed("frust-acme-tag"),
                },
                Contribution::PlistEntry {
                    key: Cow::Borrowed("NSBluetoothAlwaysUsageDescription"),
                    value: Cow::Borrowed("Finds nearby Acme tags."),
                    comment: Cow::Borrowed("test"),
                },
            ]),
            ..crate::plugin::PluginSpec::DEFAULT
        });
        let err = pending_contributions(&app, &entry, &[]).unwrap_err();
        assert!(
            matches!(&err, PluginAddError::MissingProjectFile(rel) if rel == apply::PLIST_REL),
            "{err:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_collision_names_both_plugins() {
        let collision = ModuleCollision {
            kind: NativeKind::GradleModule {
                gradle_name: Cow::Borrowed(":frust-acme"),
            },
            kept_plugin_id: "acme-a".into(),
            dropped_plugin_id: "acme-b".into(),
        };
        assert_eq!(
            collision.to_string(),
            "Android module `:frust-acme`: declared by plugins `acme-a` and `acme-b`; \
             `acme-a`'s is wired, `acme-b`'s is not"
        );
    }
}
