//! [`PluginCatalog`]: the one list of plugins every consumer looks a plugin
//! up in — `frust plugin add`, the TUI's Add Plugin dialog, the machine-local
//! wiring ([`crate::platform_wiring`]) and the desktop-lane scan
//! ([`super::desktop_contributions`]).
//!
//! The built-in registry ([`known_plugins`]) is one source of entries; a
//! plugin described at run time is another, carried beside it with its own
//! [`PluginOrigin`]. Lookups never care which source an entry came from;
//! only the code paths that need a package layout do, and they say so.
//!
//! [`project_catalog`] assembles a project's catalog — the built-in registry
//! plus what discovery finds in its dependency graph — and returns what a
//! front end should warn about as [`CatalogWarning`] values, never printed.

use std::fmt;
use std::path::{Path, PathBuf};

use super::manifest::{ManifestError, discover};
use super::registry::known_plugins;
use super::{NativeModule, PluginSpec};
use crate::packages::{GraphLocator, PackagesError};

/// Where a [`CatalogEntry`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginOrigin {
    /// The static in-crate registry ([`known_plugins`]): the plugin lives
    /// under `plugins/<crate_dir>/` of a frust checkout or release.
    BuiltIn,
    /// A package that describes itself as a frust plugin in its own manifest.
    Manifest {
        /// The cargo package name.
        package: String,
        /// The directory cargo resolved the package to.
        package_dir: PathBuf,
        /// The package version cargo resolved.
        version: String,
        /// How the project depends on the package.
        source: DepSource,
    },
}

/// How a project depends on a manifest-described plugin package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepSource {
    /// A `path` dependency.
    Path,
    /// A crates.io dependency: a bare version requirement names it.
    Registry,
    /// A git dependency or one from an alternate registry: a bare version
    /// requirement would name a different source, so `add_plugin` never
    /// writes its dependency line.
    Other,
}

impl DepSource {
    /// The source cargo metadata reports for a package: `None` is a path
    /// dependency (or a workspace member), a crates.io index (git or sparse
    /// protocol) is [`DepSource::Registry`], anything else is
    /// [`DepSource::Other`].
    pub(crate) fn from_cargo_source(source: Option<&str>) -> Self {
        const CRATES_IO: [&str; 2] = [
            "registry+https://github.com/rust-lang/crates.io-index",
            "sparse+https://index.crates.io/",
        ];
        match source {
            None => Self::Path,
            Some(source) if CRATES_IO.contains(&source) => Self::Registry,
            Some(_) => Self::Other,
        }
    }
}

/// One plugin in a [`PluginCatalog`]: its description and where that
/// description came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The plugin's description.
    pub spec: PluginSpec,
    /// Where `spec` came from.
    pub origin: PluginOrigin,
}

impl CatalogEntry {
    /// A built-in registry entry.
    pub fn built_in(spec: PluginSpec) -> Self {
        Self {
            spec,
            origin: PluginOrigin::BuiltIn,
        }
    }

    /// Whether this entry came from the built-in registry.
    pub fn is_built_in(&self) -> bool {
        self.origin == PluginOrigin::BuiltIn
    }

    /// Every native module this plugin can contribute, base and optional
    /// features alike, like [`PluginSpec::native_modules`] — but located by
    /// this entry's origin: a built-in path sits under `plugins/<crate_dir>/`,
    /// a manifest-described one is already relative to its package.
    pub fn native_modules(&self) -> Vec<NativeModule> {
        self.spec
            .base
            .iter()
            .chain(
                self.spec
                    .optional_features
                    .iter()
                    .flat_map(|f| f.contributions.iter()),
            )
            .filter_map(|contribution| {
                super::apply::native_module_in(&self.spec, &self.origin, contribution)?.ok()
            })
            .collect()
    }
}

/// An ordered list of plugins with unique ids against the built-in registry:
/// the built-in entries first, in registry order, then any described at run
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCatalog {
    entries: Vec<CatalogEntry>,
}

impl PluginCatalog {
    /// Exactly the built-in registry, in [`known_plugins`] order.
    pub fn built_in() -> Self {
        Self {
            entries: known_plugins()
                .into_iter()
                .map(CatalogEntry::built_in)
                .collect(),
        }
    }

    /// `built_in` followed by `discovered`, in the order given, minus every
    /// discovered entry whose id `built_in` already has: the earlier entry
    /// always wins. Returns the catalog and the ids dropped, in the order
    /// they were dropped, so a caller can warn about each. Ids repeated
    /// among `discovered` themselves are the discoverer's to resolve.
    pub fn with_discovered(
        built_in: PluginCatalog,
        discovered: Vec<CatalogEntry>,
    ) -> (Self, Vec<String>) {
        let mut entries = built_in.entries;
        let base = entries.len();
        let mut dropped = Vec::new();
        for entry in discovered {
            if entries[..base].iter().any(|e| e.spec.id == entry.spec.id) {
                dropped.push(entry.spec.id.into_owned());
            } else {
                entries.push(entry);
            }
        }
        (Self { entries }, dropped)
    }

    /// The first entry with plugin id `id`.
    pub fn find(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| e.spec.id == id)
    }

    /// Every entry, in catalog order.
    pub fn iter(&self) -> std::slice::Iter<'_, CatalogEntry> {
        self.entries.iter()
    }

    /// Every plugin description, in catalog order.
    pub fn specs(&self) -> impl Iterator<Item = &PluginSpec> {
        self.entries.iter().map(|e| &e.spec)
    }
}

impl<'a> IntoIterator for &'a PluginCatalog {
    type Item = &'a CatalogEntry;
    type IntoIter = std::slice::Iter<'a, CatalogEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Something a front end warns about while assembling a project's catalog
/// ([`project_catalog`]); none of them fails the assembly. `Display` is the
/// whole warning line, `warning: ` included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogWarning {
    /// A package's plugin description was refused; the package is skipped.
    Manifest(ManifestError),
    /// A discovered plugin was dropped because a built-in plugin has its id.
    ShadowedByBuiltIn {
        /// The plugin id both declare.
        id: String,
        /// The package whose description was dropped.
        package: String,
    },
    /// Discovery failed, so the catalog is the built-in registry alone
    /// ([`project_catalog_or_built_in`]).
    DiscoveryFailed(PackagesError),
}

impl fmt::Display for CatalogWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(problem) => write!(f, "warning: {problem}"),
            Self::ShadowedByBuiltIn { id, package } => write!(
                f,
                "warning: plugin `{id}` described by package `{package}` is ignored: a built-in \
                 plugin has the same id"
            ),
            Self::DiscoveryFailed(err) => write!(
                f,
                "warning: could not discover the plugins this project's dependencies describe: \
                 {err}"
            ),
        }
    }
}

/// A project's assembled catalog and what to warn about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCatalog {
    /// The built-in registry followed by every discovered plugin kept.
    pub catalog: PluginCatalog,
    /// In order: each refused description, then each shadowed id.
    pub warnings: Vec<CatalogWarning>,
}

/// The plugin catalog of the project at `project_dir`: the built-in registry
/// followed by every plugin a package of its resolved dependency graph
/// describes in its own manifest ([`discover`], one graph read through
/// `locator`). A refused description and a discovered plugin dropped because
/// a built-in has its id ([`PluginCatalog::with_discovered`]) are each one
/// warning; neither fails. A failed discovery is the locator's error.
pub fn project_catalog(
    locator: &dyn GraphLocator,
    project_dir: &Path,
) -> Result<ProjectCatalog, PackagesError> {
    let discovery = discover(project_dir, locator)?;
    let mut warnings: Vec<CatalogWarning> = discovery
        .problems
        .into_iter()
        .map(CatalogWarning::Manifest)
        .collect();
    let packages: Vec<(String, String)> = discovery
        .plugins
        .iter()
        .map(|entry| (entry.spec.id.to_string(), package_of(entry).to_string()))
        .collect();
    let (catalog, dropped) =
        PluginCatalog::with_discovered(PluginCatalog::built_in(), discovery.plugins);
    warnings.extend(dropped.into_iter().map(|id| {
        let package = packages
            .iter()
            .find(|(found, _)| *found == id)
            .map_or_else(String::new, |(_, package)| package.clone());
        CatalogWarning::ShadowedByBuiltIn { id, package }
    }));
    Ok(ProjectCatalog { catalog, warnings })
}

/// [`project_catalog`], falling back to the built-in registry when discovery
/// fails, with one [`CatalogWarning::DiscoveryFailed`] saying so. A project
/// without a `Cargo.toml` falls back silently: there is nothing to discover,
/// and the caller reports the missing manifest itself.
pub fn project_catalog_or_built_in(
    locator: &dyn GraphLocator,
    project_dir: &Path,
) -> ProjectCatalog {
    match project_catalog(locator, project_dir) {
        Ok(found) => found,
        Err(err) => ProjectCatalog {
            catalog: PluginCatalog::built_in(),
            warnings: match err {
                PackagesError::NoManifest { .. } => Vec::new(),
                err => vec![CatalogWarning::DiscoveryFailed(err)],
            },
        },
    }
}

/// The package a manifest-described entry comes from; empty for a built-in.
pub(crate) fn package_of(entry: &CatalogEntry) -> &str {
    match &entry.origin {
        PluginOrigin::BuiltIn => "",
        PluginOrigin::Manifest { package, .. } => package,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::CargoLocator;
    use crate::plugin::{Contribution, FeatureSpec};
    use crate::process::{FakeProcessRunner, Output};
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A spec built entirely at run time: every string is owned.
    fn owned_spec(id: &str) -> PluginSpec {
        PluginSpec {
            id: Cow::Owned(id.to_string()),
            summary: Cow::Owned(format!("{id} described at run time")),
            crate_dir: Cow::Owned(id.to_string()),
            base: Cow::Owned(vec![Contribution::CargoDep {
                name: Cow::Owned(format!("frust-{id}")),
            }]),
            optional_features: Cow::Owned(vec![FeatureSpec {
                id: Cow::Owned("extra".to_string()),
                contributions: Cow::Owned(vec![Contribution::CargoFeature {
                    name: Cow::Owned(format!("frust-{id}")),
                    feature: Cow::Owned("extra".to_string()),
                }]),
                ..FeatureSpec::DEFAULT
            }]),
            ..PluginSpec::DEFAULT
        }
    }

    fn manifest(spec: PluginSpec) -> CatalogEntry {
        CatalogEntry {
            origin: PluginOrigin::Manifest {
                package: format!("frust-{}", spec.id),
                package_dir: PathBuf::from("/tmp/somewhere"),
                version: "0.1.0".to_string(),
                source: DepSource::Path,
            },
            spec,
        }
    }

    #[test]
    fn built_in_is_the_registry_in_order() {
        let catalog = PluginCatalog::built_in();
        let ids: Vec<&str> = catalog.specs().map(|s| &*s.id).collect();
        let registry: Vec<String> = known_plugins()
            .into_iter()
            .map(|s| s.id.into_owned())
            .collect();
        assert_eq!(ids, registry);
        assert!(catalog.iter().all(CatalogEntry::is_built_in));
        assert_eq!(
            catalog.find("secure-storage").map(|e| &e.spec),
            known_plugins().iter().find(|s| s.id == "secure-storage")
        );
    }

    #[test]
    fn an_owned_spec_is_found_by_id() {
        let id = String::from("run-time");
        let (catalog, dropped) = PluginCatalog::with_discovered(
            PluginCatalog::built_in(),
            vec![manifest(owned_spec(&id))],
        );
        assert!(dropped.is_empty());
        let entry = catalog
            .find(&id)
            .expect("the discovered entry is in the catalog");
        assert_eq!(entry.spec, owned_spec("run-time"));
        assert!(!entry.is_built_in());
        assert!(matches!(entry.spec.id, Cow::Owned(_)));
        // It comes after every built-in entry.
        assert_eq!(catalog.iter().count(), known_plugins().len() + 1);
        assert_eq!(catalog.iter().last(), Some(entry));
    }

    #[test]
    fn a_discovered_entry_with_a_built_in_id_is_dropped_and_reported() {
        let (catalog, dropped) = PluginCatalog::with_discovered(
            PluginCatalog::built_in(),
            vec![
                manifest(owned_spec("camera")),
                manifest(owned_spec("fresh")),
                manifest(owned_spec("iap")),
            ],
        );
        assert_eq!(dropped, ["camera", "iap"]);
        assert!(catalog.find("camera").unwrap().is_built_in());
        assert!(catalog.find("iap").unwrap().is_built_in());
        assert!(!catalog.find("fresh").unwrap().is_built_in());
        assert_eq!(catalog.iter().count(), known_plugins().len() + 1);
    }

    #[test]
    fn an_unknown_id_is_not_found() {
        assert!(PluginCatalog::built_in().find("nope").is_none());
    }

    #[test]
    fn only_a_crates_io_source_is_a_registry_dependency() {
        let cases = [
            (None, DepSource::Path),
            (
                Some("registry+https://github.com/rust-lang/crates.io-index"),
                DepSource::Registry,
            ),
            (Some("sparse+https://index.crates.io/"), DepSource::Registry),
            (
                Some("git+https://example.com/acme/frust-acme-ble?branch=main#0123abcd"),
                DepSource::Other,
            ),
            (
                Some("sparse+https://registry.example.com/index/"),
                DepSource::Other,
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(DepSource::from_cargo_source(source), expected, "{source:?}");
        }
    }

    #[test]
    fn a_manifest_entry_locates_its_modules_inside_its_package() {
        let mut spec = owned_spec("acme-ble");
        spec.base = Cow::Owned(vec![
            Contribution::CargoDep {
                name: Cow::Owned("frust-acme-ble".into()),
            },
            Contribution::GradleModule {
                gradle_name: Cow::Owned(":frust-acme-ble".into()),
                rel_path: Cow::Owned("platform/android".into()),
            },
        ]);
        let entry = manifest(spec.clone());
        assert_eq!(
            entry.native_modules(),
            [NativeModule {
                package: Cow::Owned("frust-acme-ble".into()),
                dir_in_package: Cow::Owned("platform/android".into()),
                kind: crate::plugin::NativeKind::GradleModule {
                    gradle_name: Cow::Owned(":frust-acme-ble".into()),
                },
            }]
        );
        // The same path read as a built-in one lies outside `plugins/<dir>/`.
        assert!(spec.native_modules().is_empty());
        assert!(CatalogEntry::built_in(spec).native_modules().is_empty());
    }

    /// A project directory holding only a `Cargo.toml`, so the locator's
    /// up-front manifest check passes and the fake runner is reached.
    fn project(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "frust-drive-catalog-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"acme_app\"\n").unwrap();
        dir.canonicalize().unwrap()
    }

    /// A runner answering the project's full-graph `cargo metadata` with the
    /// app itself plus one package per `(name, plugin table)` of `plugins`.
    fn runner(project: &Path, plugins: &[(&str, &str)]) -> FakeProcessRunner {
        let package = |name: &str, dir: &Path, metadata: &str| {
            let manifest = dir
                .join("Cargo.toml")
                .display()
                .to_string()
                .replace('\\', "\\\\");
            format!(
                r#"{{"name":"{name}","version":"0.1.0","id":"{name}@0.1.0","source":null,"manifest_path":"{manifest}","features":{{}},"metadata":{metadata},"dependencies":[]}}"#
            )
        };
        let mut packages = vec![package("acme_app", project, "null")];
        for (name, table) in plugins {
            let metadata = format!(r#"{{"frust":{{"plugin":{table}}}}}"#);
            packages.push(package(name, &project.join(name), &metadata));
        }
        let document = format!(
            r#"{{"packages":[{}],"workspace_members":["acme_app@0.1.0"],"resolve":null,"version":1}}"#,
            packages.join(",")
        );
        FakeProcessRunner::new().with(metadata_key(project), ok(&document))
    }

    fn metadata_key(project: &Path) -> String {
        format!(
            "cargo metadata --format-version 1 --manifest-path {}",
            project.join("Cargo.toml").display()
        )
    }

    fn ok(stdout: &str) -> Output {
        Output {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    const ACME: (&str, &str) = (
        "frust-acme-ble",
        r#"{"id":"acme-ble","summary":"Acme BLE"}"#,
    );

    #[test]
    fn a_discovered_plugin_follows_the_built_in_ones() {
        let dir = project("discovered");
        let runner = runner(&dir, &[ACME]);
        let found = project_catalog(&CargoLocator::new(&runner), &dir).unwrap();
        assert_eq!(found.warnings, []);
        assert_eq!(found.catalog.iter().count(), known_plugins().len() + 1);
        let entry = found.catalog.iter().last().unwrap();
        assert_eq!(entry.spec.id, "acme-ble");
        assert_eq!(package_of(entry), "frust-acme-ble");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_description_is_a_warning_and_the_catalog_is_still_built() {
        let dir = project("refused");
        let bad = ("frust-acme-bad", r#"{"id":"Acme_Bad","summary":"Acme"}"#);
        let runner = runner(&dir, &[ACME, bad]);
        let found = project_catalog(&CargoLocator::new(&runner), &dir).unwrap();
        assert_eq!(found.warnings.len(), 1, "{:?}", found.warnings);
        assert!(
            matches!(&found.warnings[0], CatalogWarning::Manifest(problem) if problem.package == "frust-acme-bad"),
            "{:?}",
            found.warnings
        );
        assert!(
            found.warnings[0]
                .to_string()
                .starts_with("warning: plugin manifest of package `frust-acme-bad`: "),
            "{}",
            found.warnings[0]
        );
        assert!(found.catalog.find("acme-ble").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discovered_id_a_built_in_has_is_dropped_with_a_warning() {
        let dir = project("shadowed");
        let cam = (
            "frust-acme-cam",
            r#"{"id":"camera","summary":"Acme camera"}"#,
        );
        let runner = runner(&dir, &[ACME, cam]);
        let found = project_catalog(&CargoLocator::new(&runner), &dir).unwrap();
        assert_eq!(
            found.warnings,
            [CatalogWarning::ShadowedByBuiltIn {
                id: "camera".into(),
                package: "frust-acme-cam".into(),
            }]
        );
        assert_eq!(
            found.warnings[0].to_string(),
            "warning: plugin `camera` described by package `frust-acme-cam` is ignored: a \
             built-in plugin has the same id"
        );
        assert!(found.catalog.find("camera").unwrap().is_built_in());
        assert!(found.catalog.find("acme-ble").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_metadata_run_falls_back_to_the_built_in_plugins_with_one_warning() {
        let dir = project("fails");
        let runner = FakeProcessRunner::new().with(
            metadata_key(&dir),
            Output {
                success: false,
                stdout: String::new(),
                stderr: "error: no matching package named `frust-ui` found".to_string(),
            },
        );
        let locator = CargoLocator::new(&runner);
        assert!(matches!(
            project_catalog(&locator, &dir),
            Err(PackagesError::MetadataFailed { .. })
        ));
        let found = project_catalog_or_built_in(&locator, &dir);
        assert_eq!(found.catalog, PluginCatalog::built_in());
        assert_eq!(found.warnings.len(), 1, "{:?}", found.warnings);
        assert!(matches!(
            &found.warnings[0],
            CatalogWarning::DiscoveryFailed(PackagesError::MetadataFailed { .. })
        ));
        let line = found.warnings[0].to_string();
        assert!(
            line.starts_with(
                "warning: could not discover the plugins this project's dependencies describe: "
            ),
            "{line}"
        );
        assert!(line.contains("no matching package"), "{line}");

        // No manifest at all: the built-in plugins, silently.
        std::fs::remove_file(dir.join("Cargo.toml")).unwrap();
        let found = project_catalog_or_built_in(&locator, &dir);
        assert_eq!(found.catalog, PluginCatalog::built_in());
        assert_eq!(found.warnings, []);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
