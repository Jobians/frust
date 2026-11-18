//! Where a project's dependency packages live on disk, answered by cargo.
//!
//! Post-scaffold code that needs a file shipped inside a framework crate — the
//! browser host page in `frust-shell-web`, a plugin's Gradle module or Swift
//! package — asks cargo where that package is rather than deriving the
//! location from the project's `frust` dependency. The question then has one
//! answer whatever form the dependency takes: a path into a checkout resolves
//! to the checkout's crate directory, and a crates.io version resolves to the
//! unpacked crate under cargo's registry source cache.
//!
//! [`PackageLocator`] is the seam. [`CargoLocator`] runs `cargo metadata`
//! through an injected [`ProcessRunner`]; [`CachedLocator`] remembers each
//! answer so a caller asking twice runs cargo once; `StubLocator` (built under
//! `test` or the `test-util` feature) answers from a map, so a fixture test
//! never spawns cargo against a fake project. [`locate`] and [`locate_many`]
//! are the cargo-backed shorthand for a caller holding no runner.
//!
//! [`GraphLocator`] is the wider seam plugin discovery reads through: the
//! whole resolved graph ([`PackageGraph`]) as well as package directories,
//! both derived from one [`GraphMetadata`] document. A [`CachedLocator`] over
//! a graph-reading locator keeps that document per project, so discovery and
//! the wiring a build runs after it share one `cargo metadata` run.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::facade_key;
use crate::process::{ProcessRunner, RealProcessRunner};

/// Why a package directory could not be resolved — a library contract callers
/// wrap into their own error enums (`docs/CODE_STANDARDS.md`'s Error
/// Handling convention).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackagesError {
    /// The directory holds no `Cargo.toml`, so there is no dependency graph
    /// to ask about — checked before cargo is spawned.
    #[error(
        "'{}' has no Cargo.toml, so cargo has no dependency graph to locate packages in",
        project_dir.display()
    )]
    NoManifest { project_dir: PathBuf },
    /// `cargo` itself could not be run.
    #[error(
        "could not run `cargo metadata` for '{}': {reason} — is cargo installed and on PATH?",
        project_dir.display()
    )]
    CargoUnavailable {
        project_dir: PathBuf,
        reason: String,
    },
    /// `cargo metadata` ran and failed; `stderr` is cargo's own explanation.
    /// The message adds a network hint only when cargo's text is about the
    /// index, a download or the network ([`metadata_failure_hint`]); any other
    /// failure (a stale lock, a moved path dependency) is cargo's error alone.
    #[error(
        "`cargo metadata` failed for '{}'{}:\n{stderr}",
        project_dir.display(),
        metadata_failure_hint(stderr)
    )]
    MetadataFailed {
        project_dir: PathBuf,
        stderr: String,
    },
    /// `cargo metadata` succeeded but its output did not have the documented
    /// `--format-version 1` shape.
    #[error("`cargo metadata` for '{}' printed unreadable output: {reason}", project_dir.display())]
    MetadataUnreadable {
        project_dir: PathBuf,
        reason: String,
    },
    /// Two versions of the package are both reachable from the project's root
    /// package, so no single directory answers; `candidates` renders each as
    /// `name@version (directory)`.
    #[error(
        "package `{package}` resolves to more than one version in the dependency graph of '{}': \
         {}",
        project_dir.display(),
        candidates.join(", ")
    )]
    Ambiguous {
        project_dir: PathBuf,
        package: String,
        candidates: Vec<String>,
    },
    /// The package is not in the project's dependency graph.
    /// `frust_dependency` is [`describe_frust_dependency`]'s rendering of how
    /// the project depends on the framework, since that dependency is what
    /// brings every framework package into the graph.
    #[error(
        "package `{package}` is not in the dependency graph of '{}' — the project's frust \
         dependency is {frust_dependency}",
        project_dir.display()
    )]
    PackageAbsent {
        project_dir: PathBuf,
        package: String,
        frust_dependency: String,
    },
}

/// The text between "failed for '<dir>'" and cargo's stderr: a network hint
/// when the stderr is about the registry index, a download or the network,
/// nothing otherwise — a stale lock must not be blamed on connectivity.
fn metadata_failure_hint(stderr: &str) -> &'static str {
    const NETWORK_MARKERS: [&str; 7] = [
        "failed to download",
        "failed to fetch",
        "unable to update registry",
        "spurious network error",
        "could not resolve host",
        "network",
        "download",
    ];
    let lower = stderr.to_lowercase();
    if NETWORK_MARKERS.iter().any(|marker| lower.contains(marker)) {
        " — a project that depends on the published frust crates needs network access once, \
         to download them"
    } else {
        ""
    }
}

/// Resolves package names to the directories holding their `Cargo.toml`.
pub trait PackageLocator {
    /// The directory of each of `packages`, in the order given, as resolved in
    /// the dependency graph of the Cargo project at `project_dir`. Fails as a
    /// whole if any one of them cannot be resolved.
    fn locate_many(
        &self,
        project_dir: &Path,
        packages: &[&str],
    ) -> Result<Vec<PathBuf>, PackagesError>;

    /// [`locate_many`](Self::locate_many) for a single package.
    fn locate(&self, project_dir: &Path, package: &str) -> Result<PathBuf, PackagesError> {
        let mut dirs = self.locate_many(project_dir, &[package])?;
        dirs.pop().ok_or_else(|| PackagesError::MetadataUnreadable {
            project_dir: project_dir.to_path_buf(),
            reason: format!("no directory came back for `{package}`"),
        })
    }

    /// This locator as a [`GraphLocator`], when it reads the whole dependency
    /// graph itself; `None` by default. [`CachedLocator::new`] asks, so a
    /// cache over a graph-reading locator serves the graph and every
    /// directory from one document.
    fn as_graph_locator(&self) -> Option<&dyn GraphLocator> {
        None
    }
}

/// A [`PackageLocator`] that also reads the project's whole resolved
/// dependency graph — the seam plugin discovery takes. Both answers derive
/// from one [`GraphMetadata`] document, so an implementor provides only
/// [`graph_metadata`](Self::graph_metadata).
pub trait GraphLocator: PackageLocator {
    /// The full-graph `cargo metadata` document of the Cargo project at
    /// `project_dir`.
    fn graph_metadata(&self, project_dir: &Path) -> Result<GraphMetadata, PackagesError>;

    /// Every package of the resolved dependency graph of the project at
    /// `project_dir`, read from [`graph_metadata`](Self::graph_metadata).
    fn package_graph(&self, project_dir: &Path) -> Result<PackageGraph, PackagesError> {
        self.graph_metadata(project_dir)?.package_graph()
    }
}

/// One project's full-graph `cargo metadata --format-version 1` document, as
/// cargo printed it: the read a package directory and the package graph are
/// both answered from. Cheap to clone; the text is shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphMetadata {
    project_dir: PathBuf,
    document: Arc<str>,
}

impl GraphMetadata {
    /// The document `document` printed for the project at `project_dir`.
    pub fn new(project_dir: impl Into<PathBuf>, document: impl Into<Arc<str>>) -> Self {
        Self {
            project_dir: project_dir.into(),
            document: document.into(),
        }
    }

    /// The directory of each of `packages`, in the order given — exactly the
    /// answer [`CargoLocator`]'s [`locate_many`](PackageLocator::locate_many)
    /// gives for this document, errors included.
    pub fn locate_many(&self, packages: &[&str]) -> Result<Vec<PathBuf>, PackagesError> {
        packages_from_metadata(&self.project_dir, &self.document, packages)
    }

    /// Every package of the document's resolved graph.
    pub fn package_graph(&self) -> Result<PackageGraph, PackagesError> {
        package_graph_from_metadata(&self.project_dir, &self.document)
    }
}

/// How [`CargoLocator`] treats the project's `Cargo.lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LockfilePolicy {
    /// Resolve exactly as the following `cargo build` would: no `--locked`, so
    /// cargo updates `Cargo.lock` when the manifest changed since it was
    /// written (a dependency added after the first build) instead of failing.
    #[default]
    AllowUpdate,
    /// Pass `--locked`: resolving never touches the lockfile and fails if it
    /// is out of date. For a caller that must not modify the project; no
    /// caller uses it today.
    Locked,
}

/// The real [`PackageLocator`]: one `cargo metadata --format-version 1` run
/// (dependencies included, unlike the `--no-deps` target-directory probes
/// elsewhere in this crate) per [`locate_many`](PackageLocator::locate_many)
/// call, through the injected runner.
pub struct CargoLocator<'r> {
    runner: &'r dyn ProcessRunner,
    lockfile: LockfilePolicy,
}

impl<'r> CargoLocator<'r> {
    pub fn new(runner: &'r dyn ProcessRunner) -> Self {
        Self {
            runner,
            lockfile: LockfilePolicy::AllowUpdate,
        }
    }

    /// The same locator under `policy`.
    pub fn with_lockfile_policy(mut self, policy: LockfilePolicy) -> Self {
        self.lockfile = policy;
        self
    }

    /// The full-graph `cargo` argv for `project_dir`'s manifest at
    /// `manifest`.
    #[cfg(test)]
    fn metadata_args(&self, manifest: &Path) -> Vec<String> {
        self.metadata_args_with(manifest, MetadataScope::Graph)
    }

    /// The `cargo` argv for `project_dir`'s manifest at `manifest` under
    /// `scope`: the one `cargo metadata` shape this locator spawns,
    /// `--no-deps` added for a [`MetadataScope::Members`] read.
    fn metadata_args_with(&self, manifest: &Path, scope: MetadataScope) -> Vec<String> {
        let mut args = vec![
            "metadata".to_string(),
            "--format-version".to_string(),
            "1".to_string(),
        ];
        if scope == MetadataScope::Members {
            args.push("--no-deps".to_string());
        }
        args.push("--manifest-path".to_string());
        args.push(manifest.to_string_lossy().into_owned());
        if self.lockfile == LockfilePolicy::Locked {
            args.push("--locked".to_string());
        }
        args
    }

    /// Runs `cargo metadata` for the project at `project_dir` under `scope`
    /// and returns its stdout, after the up-front manifest check every read
    /// shares.
    fn run_metadata(
        &self,
        project_dir: &Path,
        scope: MetadataScope,
    ) -> Result<String, PackagesError> {
        let manifest = project_dir.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(PackagesError::NoManifest {
                project_dir: project_dir.to_path_buf(),
            });
        }
        let args = self.metadata_args_with(&manifest, scope);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out =
            self.runner
                .run("cargo", &argv)
                .map_err(|err| PackagesError::CargoUnavailable {
                    project_dir: project_dir.to_path_buf(),
                    reason: format!("{err:#}"),
                })?;
        if !out.success {
            return Err(PackagesError::MetadataFailed {
                project_dir: project_dir.to_path_buf(),
                stderr: out.stderr.trim().to_string(),
            });
        }
        Ok(out.stdout)
    }

    /// The declared dependencies of the root package of the project at
    /// `project_dir`, read with `cargo metadata --no-deps` — cargo's own view
    /// of the manifest, so a `workspace = true` entry carries the rename its
    /// workspace declares. No dependency graph is resolved, so nothing is
    /// downloaded.
    pub(crate) fn root_dependencies(
        &self,
        project_dir: &Path,
    ) -> Result<Vec<MetadataDependency>, PackagesError> {
        let stdout = self.run_metadata(project_dir, MetadataScope::Members)?;
        root_dependencies_from_metadata(project_dir, &stdout)
    }
}

/// How much of the project a `cargo metadata` run reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataScope {
    /// The full resolved dependency graph — what locating a package needs.
    Graph,
    /// Only the workspace members' own manifests (`--no-deps`).
    Members,
}

impl PackageLocator for CargoLocator<'_> {
    fn locate_many(
        &self,
        project_dir: &Path,
        packages: &[&str],
    ) -> Result<Vec<PathBuf>, PackagesError> {
        self.graph_metadata(project_dir)?.locate_many(packages)
    }

    fn as_graph_locator(&self) -> Option<&dyn GraphLocator> {
        Some(self)
    }
}

/// The package graph comes from the same `cargo metadata` run (argv,
/// lockfile policy and runner) that [`locate_many`](PackageLocator::locate_many)
/// makes.
impl GraphLocator for CargoLocator<'_> {
    fn graph_metadata(&self, project_dir: &Path) -> Result<GraphMetadata, PackagesError> {
        let stdout = self.run_metadata(project_dir, MetadataScope::Graph)?;
        Ok(GraphMetadata::new(project_dir, stdout))
    }
}

/// One `dependencies[]` entry of a package in a `cargo metadata` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetadataDependency {
    /// The depended-on package's name.
    pub(crate) name: String,
    /// The key the manifest gives it, when that differs from `name`
    /// (`ui = { package = "frust-ui" }` → `Some("ui")`).
    pub(crate) rename: Option<String>,
    /// `None` for a normal dependency, `Some("dev")`/`Some("build")`
    /// otherwise.
    pub(crate) kind: Option<String>,
}

/// The root package's `dependencies[]` out of a `cargo metadata
/// --format-version 1` document: the package whose manifest sits in
/// `project_dir`, or the only package listed. A virtual workspace manifest
/// has no root package and answers an empty list. Split from
/// [`CargoLocator::root_dependencies`] so the parse is tested against canned
/// JSON rather than a cargo run.
pub(crate) fn root_dependencies_from_metadata(
    project_dir: &Path,
    metadata: &str,
) -> Result<Vec<MetadataDependency>, PackagesError> {
    let unreadable = |reason: String| PackagesError::MetadataUnreadable {
        project_dir: project_dir.to_path_buf(),
        reason,
    };
    let doc: serde_json::Value =
        serde_json::from_str(metadata).map_err(|err| unreadable(err.to_string()))?;
    let listed = doc
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| unreadable("no `packages` array".to_string()))?;
    let identity = |dir: &Path| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let wanted = identity(project_dir);
    let root = listed
        .iter()
        .find(|entry| {
            entry
                .get("manifest_path")
                .and_then(serde_json::Value::as_str)
                .and_then(|manifest| Path::new(manifest).parent())
                .is_some_and(|dir| identity(dir) == wanted)
        })
        .or(match listed.as_slice() {
            [only] => Some(only),
            _ => None,
        });
    let Some(root) = root else {
        return Ok(Vec::new());
    };
    let str_of = |entry: &serde_json::Value, key: &str| {
        entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    root.get("dependencies")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|dep| {
            let name = str_of(dep, "name")
                .ok_or_else(|| unreadable("a dependency carries no `name`".to_string()))?;
            Ok(MetadataDependency {
                name,
                rename: str_of(dep, "rename"),
                kind: str_of(dep, "kind"),
            })
        })
        .collect()
}

/// Picks `packages` out of a `cargo metadata --format-version 1` document:
/// each name's `packages[]` entry, as the parent of its `manifest_path`. When
/// several packages share a name (two versions in one graph), the entry
/// reachable from the root package through `resolve` — the version cargo
/// actually builds the project against — wins; two reachable versions are
/// [`PackagesError::Ambiguous`]. Split from [`CargoLocator`] so the parse is
/// tested against canned JSON rather than a cargo run.
pub(crate) fn packages_from_metadata(
    project_dir: &Path,
    metadata: &str,
    packages: &[&str],
) -> Result<Vec<PathBuf>, PackagesError> {
    let unreadable = |reason: String| PackagesError::MetadataUnreadable {
        project_dir: project_dir.to_path_buf(),
        reason,
    };
    let doc: serde_json::Value =
        serde_json::from_str(metadata).map_err(|err| unreadable(err.to_string()))?;
    let listed = doc
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| unreadable("no `packages` array".to_string()))?;
    let str_of = |entry: &serde_json::Value, key: &str| {
        entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };

    let mut reachable: Option<HashSet<String>> = None;
    let mut dirs = Vec::with_capacity(packages.len());
    for wanted in packages {
        let mut candidates: Vec<&serde_json::Value> = listed
            .iter()
            .filter(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(*wanted))
            .collect();
        if candidates.len() > 1 {
            let closure = reachable.get_or_insert_with(|| root_closure(&doc));
            let in_closure: Vec<&serde_json::Value> = candidates
                .iter()
                .copied()
                .filter(|entry| str_of(entry, "id").is_some_and(|id| closure.contains(&id)))
                .collect();
            if in_closure.len() == 1 {
                candidates = in_closure;
            } else {
                // Zero reachable means `resolve` could not discriminate
                // (virtual workspace, absent graph): every candidate is
                // equally plausible, which is the same ambiguity.
                let named = if in_closure.is_empty() {
                    candidates
                } else {
                    in_closure
                };
                return Err(PackagesError::Ambiguous {
                    project_dir: project_dir.to_path_buf(),
                    package: (*wanted).to_string(),
                    candidates: named
                        .iter()
                        .map(|entry| {
                            format!(
                                "{wanted}@{} ({})",
                                str_of(entry, "version").unwrap_or_else(|| "?".into()),
                                str_of(entry, "manifest_path").unwrap_or_default()
                            )
                        })
                        .collect(),
                });
            }
        }
        let Some(entry) = candidates.first() else {
            return Err(PackagesError::PackageAbsent {
                project_dir: project_dir.to_path_buf(),
                package: (*wanted).to_string(),
                frust_dependency: describe_frust_dependency(project_dir),
            });
        };
        let manifest_path = entry
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| unreadable(format!("`{wanted}` carries no `manifest_path`")))?;
        let dir = Path::new(manifest_path)
            .parent()
            .ok_or_else(|| unreadable(format!("`{wanted}`'s manifest path has no parent")))?;
        dirs.push(dir.to_path_buf());
    }
    Ok(dirs)
}

/// One `packages[]` entry of a full-graph `cargo metadata` document, with
/// the fields plugin discovery reads. Opaque outside this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphPackage {
    /// The package name.
    pub(crate) name: String,
    /// The resolved version.
    pub(crate) version: String,
    /// `None` for a path dependency or workspace member, otherwise cargo's
    /// source string (`registry+…`, `git+…`).
    pub(crate) source: Option<String>,
    /// The package's `Cargo.toml`.
    pub(crate) manifest_path: PathBuf,
    /// The package's `[package.metadata]` table as JSON; `Null` when absent.
    pub(crate) metadata: serde_json::Value,
    /// The names of the package's `[features]`; `None` when cargo listed none.
    pub(crate) features: Option<BTreeSet<String>>,
}

impl GraphPackage {
    /// The directory holding the package's `Cargo.toml`.
    pub(crate) fn dir(&self) -> &Path {
        self.manifest_path.parent().unwrap_or(Path::new(""))
    }
}

/// Every package of a resolved dependency graph, with the project's own root
/// package marked and, when cargo resolved the graph, what the root reaches —
/// what [`GraphLocator::package_graph`] answers and plugin discovery reads.
/// Opaque outside this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageGraph {
    /// The packages, in the order cargo listed them.
    pub(crate) packages: Vec<GraphPackage>,
    /// The index in `packages` of the project's root package: the one whose
    /// manifest sits in the project directory, else `resolve.root`. `None`
    /// for a virtual workspace.
    pub(crate) root: Option<usize>,
    /// The indices in `packages` reachable from the root package through
    /// `resolve.nodes` (every dependency kind), the root included. `None`
    /// when the document carries no resolve graph or the project has no root
    /// package: nothing can then be told unreachable.
    pub(crate) closure: Option<BTreeSet<usize>>,
    /// The indices in `packages` the root package depends on directly as a
    /// normal dependency (any target); an edge that is dev-only or build-only
    /// does not count. Empty whenever `closure` is `None`.
    pub(crate) direct: BTreeSet<usize>,
}

impl PackageGraph {
    /// A graph with no resolve information: no closure, no direct set.
    pub(crate) fn new(packages: Vec<GraphPackage>, root: Option<usize>) -> Self {
        Self {
            packages,
            root,
            closure: None,
            direct: BTreeSet::new(),
        }
    }

    /// Whether the package at `index` is reachable from the root package;
    /// always `true` when the graph carries no closure.
    pub(crate) fn in_closure(&self, index: usize) -> bool {
        self.closure
            .as_ref()
            .is_none_or(|closure| closure.contains(&index))
    }

    /// Whether the package at `index` is a direct normal dependency of the
    /// root package.
    pub(crate) fn is_direct(&self, index: usize) -> bool {
        self.direct.contains(&index)
    }
}

/// Reads every package out of a `cargo metadata --format-version 1`
/// document. Split from [`GraphMetadata::package_graph`] so the parse is
/// tested against canned JSON rather than a cargo run.
pub(crate) fn package_graph_from_metadata(
    project_dir: &Path,
    metadata: &str,
) -> Result<PackageGraph, PackagesError> {
    let unreadable = |reason: String| PackagesError::MetadataUnreadable {
        project_dir: project_dir.to_path_buf(),
        reason,
    };
    let doc: serde_json::Value =
        serde_json::from_str(metadata).map_err(|err| unreadable(err.to_string()))?;
    let listed = doc
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| unreadable("no `packages` array".to_string()))?;
    let str_of = |entry: &serde_json::Value, key: &str| {
        entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let mut packages = Vec::with_capacity(listed.len());
    let mut ids = Vec::with_capacity(listed.len());
    for entry in listed {
        let name =
            str_of(entry, "name").ok_or_else(|| unreadable("a package has no `name`".into()))?;
        let field = |key: &str| {
            str_of(entry, key).ok_or_else(|| unreadable(format!("`{name}` carries no `{key}`")))
        };
        let version = field("version")?;
        let manifest_path = PathBuf::from(field("manifest_path")?);
        let features = entry
            .get("features")
            .and_then(serde_json::Value::as_object)
            .map(|features| features.keys().cloned().collect());
        ids.push(str_of(entry, "id"));
        packages.push(GraphPackage {
            source: str_of(entry, "source"),
            metadata: entry
                .get("metadata")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            name,
            version,
            manifest_path,
            features,
        });
    }
    let identity = |dir: &Path| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let wanted = identity(project_dir);
    let resolve_root = doc
        .get("resolve")
        .and_then(|resolve| resolve.get("root"))
        .and_then(serde_json::Value::as_str);
    let root = packages
        .iter()
        .position(|package| identity(package.dir()) == wanted)
        .or_else(|| {
            let root = resolve_root?;
            ids.iter().position(|id| id.as_deref() == Some(root))
        });
    let root_id = root.and_then(|index| ids[index].as_deref());
    let mut graph = PackageGraph::new(packages, root);
    if let (Some(resolve), Some(root_id)) = (resolve_of(&doc), root_id) {
        let indices_of = |wanted: &HashSet<String>| -> BTreeSet<usize> {
            ids.iter()
                .enumerate()
                .filter(|(_, id)| id.as_ref().is_some_and(|id| wanted.contains(id)))
                .map(|(index, _)| index)
                .collect()
        };
        graph.closure = Some(indices_of(&closure_from(resolve, vec![root_id])));
        graph.direct = indices_of(&direct_dependencies(resolve, root_id));
    }
    Ok(graph)
}

/// The document's `resolve` object; `None` when it is absent or `null`.
fn resolve_of(doc: &serde_json::Value) -> Option<&serde_json::Value> {
    doc.get("resolve").filter(|resolve| !resolve.is_null())
}

/// The package ids the node `root_id` of `resolve` depends on as a normal
/// dependency for any target. Read from the node's `deps` entries, whose
/// `dep_kinds` say which kinds an edge carries; an edge whose kinds are all
/// `dev` or `build` is not direct. An entry without `dep_kinds`, and a node
/// without `deps` (whose plain `dependencies` list is then used), cannot be
/// told apart by kind and counts as direct.
fn direct_dependencies(resolve: &serde_json::Value, root_id: &str) -> HashSet<String> {
    let Some(node) = resolve
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|node| node.get("id").and_then(serde_json::Value::as_str) == Some(root_id))
    else {
        return HashSet::new();
    };
    let Some(deps) = node.get("deps").and_then(serde_json::Value::as_array) else {
        return node
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
    };
    deps.iter()
        .filter(
            |dep| match dep.get("dep_kinds").and_then(serde_json::Value::as_array) {
                Some(kinds) => kinds
                    .iter()
                    .any(|kind| kind.get("kind").is_none_or(serde_json::Value::is_null)),
                None => true,
            },
        )
        .filter_map(|dep| dep.get("pkg").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The package ids reachable from the root package (every workspace member
/// for a virtual workspace, which has no root) through `resolve.nodes`.
/// Empty when the document carries no resolve graph.
fn root_closure(doc: &serde_json::Value) -> HashSet<String> {
    let Some(resolve) = resolve_of(doc) else {
        return HashSet::new();
    };
    let starts = match resolve.get("root").and_then(serde_json::Value::as_str) {
        Some(root) => vec![root],
        None => doc
            .get("workspace_members")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect(),
    };
    closure_from(resolve, starts)
}

/// The package ids reachable from `starts` (included) through the
/// `dependencies` edges of `resolve.nodes` — every dependency kind.
fn closure_from<'a>(resolve: &'a serde_json::Value, starts: Vec<&'a str>) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut deps_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in resolve
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = node.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let deps = node
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        deps_of.insert(id, deps);
    }
    let mut stack = starts;
    while let Some(id) = stack.pop() {
        if seen.insert(id.to_string()) {
            stack.extend(deps_of.get(id).into_iter().flatten().copied());
        }
    }
    seen
}

/// How the project at `project_dir` declares its framework-facade
/// dependency, for an error message: the entry as written under whichever
/// key names the facade package (`` `frust = { package = "frust-ui",
/// version = "0.5.0" }` ``, `` `frust-ui = "0.6.0"` ``), or a sentence saying
/// there is none. The facade is found by package name
/// (`facade_key::facade_entries`), so a renamed key reads the same as the
/// scaffolded `frust` one.
pub fn describe_frust_dependency(project_dir: &Path) -> String {
    let declared = fs::read_to_string(project_dir.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            let deps = doc.get("dependencies")?.as_table_like()?;
            let (key, item) = facade_key::facade_entries(deps).next()?;
            let rendered = match item {
                toml_edit::Item::Value(value) => value.to_string(),
                toml_edit::Item::Table(table) => table.clone().into_inline_table().to_string(),
                _ => return None,
            };
            Some(format!("`{key} = {}`", rendered.trim()))
        });
    declared.unwrap_or_else(|| "absent — its `[dependencies]` has no `frust` entry".to_string())
}

/// Wraps another locator and remembers its answers — errors included, as the
/// same typed [`PackagesError`] — for its own lifetime, so one pipeline run
/// that needs the same location from several places spawns cargo once.
///
/// Over a graph-reading locator ([`PackageLocator::as_graph_locator`], e.g.
/// [`CargoLocator`]) it keeps one [`GraphMetadata`] document per project and
/// answers both [`GraphLocator::package_graph`] and every
/// [`locate_many`](PackageLocator::locate_many) from it: discovery and the
/// wiring share one `cargo metadata` run. Over any other locator it
/// remembers each `(project, package)` answer, and has no package graph.
pub struct CachedLocator<'a> {
    inner: &'a dyn PackageLocator,
    answers: RefCell<HashMap<(PathBuf, String), Result<PathBuf, PackagesError>>>,
    documents: RefCell<HashMap<PathBuf, Result<GraphMetadata, PackagesError>>>,
    graphs: RefCell<HashMap<PathBuf, Result<PackageGraph, PackagesError>>>,
}

impl<'a> CachedLocator<'a> {
    pub fn new(inner: &'a dyn PackageLocator) -> Self {
        Self {
            inner,
            answers: RefCell::new(HashMap::new()),
            documents: RefCell::new(HashMap::new()),
            graphs: RefCell::new(HashMap::new()),
        }
    }

    /// The remembered document of `project_dir`, read through `graph` once.
    fn document(
        &self,
        graph: &dyn GraphLocator,
        project_dir: &Path,
    ) -> Result<GraphMetadata, PackagesError> {
        if let Some(answer) = self.documents.borrow().get(project_dir) {
            return answer.clone();
        }
        let answer = graph.graph_metadata(project_dir);
        self.documents
            .borrow_mut()
            .insert(project_dir.to_path_buf(), answer.clone());
        answer
    }
}

impl PackageLocator for CachedLocator<'_> {
    fn locate_many(
        &self,
        project_dir: &Path,
        packages: &[&str],
    ) -> Result<Vec<PathBuf>, PackagesError> {
        if let Some(graph) = self.inner.as_graph_locator() {
            return self.document(graph, project_dir)?.locate_many(packages);
        }
        let key = |package: &str| (project_dir.to_path_buf(), package.to_string());
        let unanswered: Vec<&str> = {
            let answers = self.answers.borrow();
            packages
                .iter()
                .copied()
                .filter(|package| !answers.contains_key(&key(package)))
                .collect()
        };
        if !unanswered.is_empty() {
            let resolved = self.inner.locate_many(project_dir, &unanswered);
            let mut answers = self.answers.borrow_mut();
            match resolved {
                Ok(dirs) => {
                    for (package, dir) in unanswered.iter().zip(dirs) {
                        answers.insert(key(package), Ok(dir));
                    }
                }
                Err(err) => {
                    for package in &unanswered {
                        answers.insert(key(package), Err(err.clone()));
                    }
                }
            }
        }
        let answers = self.answers.borrow();
        packages
            .iter()
            .map(|package| {
                answers.get(&key(package)).cloned().unwrap_or_else(|| {
                    Err(PackagesError::MetadataUnreadable {
                        project_dir: project_dir.to_path_buf(),
                        reason: format!("no directory came back for `{package}`"),
                    })
                })
            })
            .collect()
    }

    fn as_graph_locator(&self) -> Option<&dyn GraphLocator> {
        self.inner
            .as_graph_locator()
            .map(|_| self as &dyn GraphLocator)
    }
}

/// Over a locator that reads no graph, every read fails with
/// [`PackagesError::MetadataUnreadable`] naming why.
impl GraphLocator for CachedLocator<'_> {
    fn graph_metadata(&self, project_dir: &Path) -> Result<GraphMetadata, PackagesError> {
        match self.inner.as_graph_locator() {
            Some(graph) => self.document(graph, project_dir),
            None => Err(PackagesError::MetadataUnreadable {
                project_dir: project_dir.to_path_buf(),
                reason: "the cached locator wraps one that reads no dependency graph".to_string(),
            }),
        }
    }

    fn package_graph(&self, project_dir: &Path) -> Result<PackageGraph, PackagesError> {
        if let Some(answer) = self.graphs.borrow().get(project_dir) {
            return answer.clone();
        }
        let answer = self
            .graph_metadata(project_dir)
            .and_then(|document| document.package_graph());
        self.graphs
            .borrow_mut()
            .insert(project_dir.to_path_buf(), answer.clone());
        answer
    }
}

/// The directory of `package` in the dependency graph of the Cargo project at
/// `project_dir`, resolved by `cargo metadata` the way the next `cargo build`
/// would resolve (see [`LockfilePolicy::AllowUpdate`]).
pub fn locate(project_dir: &Path, package: &str) -> Result<PathBuf, PackagesError> {
    CargoLocator::new(&RealProcessRunner).locate(project_dir, package)
}

/// [`locate`] for several packages in one `cargo metadata` run; directories
/// come back in the order `packages` names them.
pub fn locate_many(project_dir: &Path, packages: &[&str]) -> Result<Vec<PathBuf>, PackagesError> {
    CargoLocator::new(&RealProcessRunner).locate_many(project_dir, packages)
}

/// A [`PackageLocator`] answering from a map instead of cargo — the fixture
/// seam for tests whose projects point at a fake checkout no real cargo
/// resolve could read. An unmapped package answers
/// [`PackagesError::PackageAbsent`] exactly as cargo's absence would, and
/// [`failing`](Self::failing) stands in for a `cargo metadata` failure.
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Default)]
pub struct StubLocator {
    dirs: HashMap<String, PathBuf>,
    failure: Option<String>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-util"))]
impl StubLocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers `dir` for `package`.
    pub fn with(mut self, package: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        self.dirs.insert(package.into(), dir.into());
        self
    }

    /// Fails every lookup the way a failed `cargo metadata` run does, with
    /// `stderr` as cargo's message.
    pub fn failing(stderr: impl Into<String>) -> Self {
        Self {
            failure: Some(stderr.into()),
            ..Self::default()
        }
    }

    /// How many [`locate_many`](PackageLocator::locate_many) calls reached
    /// this stub — what a real locator would have spent on cargo runs.
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(any(test, feature = "test-util"))]
impl PackageLocator for StubLocator {
    fn locate_many(
        &self,
        project_dir: &Path,
        packages: &[&str],
    ) -> Result<Vec<PathBuf>, PackagesError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(stderr) = &self.failure {
            return Err(PackagesError::MetadataFailed {
                project_dir: project_dir.to_path_buf(),
                stderr: stderr.clone(),
            });
        }
        packages
            .iter()
            .map(|package| {
                self.dirs
                    .get(*package)
                    .cloned()
                    .ok_or_else(|| PackagesError::PackageAbsent {
                        project_dir: project_dir.to_path_buf(),
                        package: (*package).to_string(),
                        frust_dependency: describe_frust_dependency(project_dir),
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeProcessRunner, Output};
    use crate::scaffold::{self, FrustDependency, TemplateContext};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "frust-drive-packages-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A trimmed `cargo metadata --format-version 1` document in cargo's
    /// real shape: the keys this module reads plus neighbours it must ignore.
    const METADATA: &str = r#"{
        "packages": [
            {
                "name": "frust-ui",
                "version": "0.5.0",
                "id": "path+file:///work/frust/crates/frust#frust-ui@0.5.0",
                "source": null,
                "manifest_path": "/work/frust/crates/frust/Cargo.toml"
            },
            {
                "name": "frust-shell-web",
                "version": "0.5.0",
                "id": "registry+https://github.com/rust-lang/crates.io-index#frust-shell-web@0.5.0",
                "source": "registry+https://github.com/rust-lang/crates.io-index",
                "manifest_path": "/home/me/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/frust-shell-web-0.5.0/Cargo.toml"
            },
            {
                "name": "frust-iap",
                "version": "0.5.0",
                "id": "path+file:///work/frust/plugins/iap#frust-iap@0.5.0",
                "source": null,
                "manifest_path": "/work/frust/plugins/iap/Cargo.toml"
            }
        ],
        "workspace_members": [],
        "resolve": null,
        "target_directory": "/work/app/target",
        "version": 1,
        "workspace_root": "/work/app"
    }"#;

    fn ok(stdout: &str) -> Output {
        Output {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    /// A project directory carrying only a manifest, so [`CargoLocator`]'s
    /// up-front manifest check passes and the runner is reached.
    fn project_with(tag: &str, manifest: &str) -> PathBuf {
        let dir = temp_dir(tag);
        fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        dir
    }

    fn metadata_key(project: &Path) -> String {
        format!(
            "cargo metadata --format-version 1 --manifest-path {}",
            project.join("Cargo.toml").display()
        )
    }

    #[test]
    fn the_metadata_parse_returns_manifest_parents_in_request_order() {
        let dirs = packages_from_metadata(
            Path::new("/work/app"),
            METADATA,
            &["frust-iap", "frust-shell-web", "frust-ui"],
        )
        .unwrap();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/work/frust/plugins/iap"),
                PathBuf::from(
                    "/home/me/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/frust-shell-web-0.5.0"
                ),
                PathBuf::from("/work/frust/crates/frust"),
            ]
        );
    }

    #[test]
    fn an_absent_package_names_the_projects_frust_dependency() {
        let project = project_with(
            "absent",
            "[package]\nname = \"a\"\n\n[dependencies]\nfrust = { package = \"frust-ui\", version = \"0.5.0\" }\n",
        );
        let err = packages_from_metadata(&project, METADATA, &["frust-camera"]).unwrap_err();
        let message = err.to_string();
        assert!(
            matches!(&err, PackagesError::PackageAbsent { package, .. } if package == "frust-camera"),
            "{err:?}"
        );
        assert!(
            message.contains(r#"`frust = { package = "frust-ui", version = "0.5.0" }`"#),
            "{message}"
        );
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn the_frust_dependency_description_covers_every_declaration_form() {
        for (tag, manifest, expected) in [
            (
                "path",
                "[dependencies]\nfrust = { package = \"frust-ui\", path = \"../frust/crates/frust\" }\n",
                r#"`frust = { package = "frust-ui", path = "../frust/crates/frust" }`"#,
            ),
            (
                "shorthand",
                "[dependencies]\nfrust = \"0.5.0\"\n",
                r#"`frust = "0.5.0"`"#,
            ),
            (
                "table",
                "[dependencies.frust]\npackage = \"frust-ui\"\nversion = \"0.5.0\"\n",
                r#"`frust = { package = "frust-ui", version = "0.5.0" }`"#,
            ),
            (
                "absent",
                "[dependencies]\nlog = \"0.4\"\n",
                "absent — its `[dependencies]` has no `frust` entry",
            ),
        ] {
            let project = project_with(tag, manifest);
            assert_eq!(describe_frust_dependency(&project), expected, "{tag}");
            let _ = fs::remove_dir_all(&project);
        }
    }

    /// The facade is found by package name: an app keying it `frust-ui` (the
    /// crates.io name) or any rename is not "absent".
    #[test]
    fn the_frust_dependency_description_finds_the_facade_under_any_key() {
        for (tag, manifest, expected) in [
            (
                "frust-ui",
                "[dependencies]\nfrust-ui = \"0.6.0\"\n",
                r#"`frust-ui = "0.6.0"`"#,
            ),
            (
                "rename",
                "[dependencies]\nlog = \"0.4\"\nui = { package = \"frust-ui\", version = \"0.6.0\" }\n",
                r#"`ui = { package = "frust-ui", version = "0.6.0" }`"#,
            ),
            (
                "other-package",
                "[dependencies]\nfrust = { package = \"not-the-facade\", version = \"1\" }\n",
                "absent — its `[dependencies]` has no `frust` entry",
            ),
        ] {
            let project = project_with(tag, manifest);
            assert_eq!(describe_frust_dependency(&project), expected, "{tag}");
            let _ = fs::remove_dir_all(&project);
        }
    }

    /// `--no-deps` lists every workspace member; the root dependencies are
    /// those of the member whose manifest sits in the project directory.
    #[test]
    fn root_dependencies_come_from_the_project_directorys_package() {
        let project = project_with("root-deps", "[package]\nname = \"app\"\n");
        let manifest = serde_json::to_string(&project.join("Cargo.toml")).unwrap();
        let metadata = format!(
            r#"{{"packages":[
                {{"name":"tool","manifest_path":"/elsewhere/tool/Cargo.toml",
                  "dependencies":[{{"name":"serde","rename":null,"kind":null}}]}},
                {{"name":"app","manifest_path":{manifest},
                  "dependencies":[
                    {{"name":"frust-ui","rename":"frust","kind":null,"target":null}},
                    {{"name":"log","rename":null,"kind":"dev","target":null}}
                  ]}}
            ],"resolve":null,"version":1}}"#
        );
        let runner = FakeProcessRunner::new().with(
            format!(
                "cargo metadata --format-version 1 --no-deps --manifest-path {}",
                project.join("Cargo.toml").display()
            ),
            ok(&metadata),
        );
        let deps = CargoLocator::new(&runner)
            .root_dependencies(&project)
            .unwrap();
        assert_eq!(
            deps,
            vec![
                MetadataDependency {
                    name: "frust-ui".into(),
                    rename: Some("frust".into()),
                    kind: None,
                },
                MetadataDependency {
                    name: "log".into(),
                    rename: None,
                    kind: Some("dev".into()),
                },
            ]
        );
        // A virtual workspace has no root package, so no root dependencies.
        let virtual_ws = r#"{"packages":[
            {"name":"a","manifest_path":"/x/a/Cargo.toml","dependencies":[]},
            {"name":"b","manifest_path":"/x/b/Cargo.toml","dependencies":[]}
        ]}"#;
        assert_eq!(
            root_dependencies_from_metadata(Path::new("/x"), virtual_ws).unwrap(),
            Vec::new()
        );
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn the_package_graph_carries_metadata_source_features_and_the_root() {
        let graph = package_graph_from_metadata(
            Path::new("/work/app"),
            r#"{"packages":[
                {"name":"lib","version":"1.0.0","id":"lib 1.0.0","source":"registry+x",
                 "manifest_path":"/reg/lib-1.0.0/Cargo.toml","metadata":{"frust":{"plugin":{}}},
                 "features":{"default":[],"extra":[]}},
                {"name":"app","version":"0.1.0","id":"app 0.1.0","source":null,
                 "manifest_path":"/elsewhere/app/Cargo.toml","metadata":null}
            ],"resolve":{"root":"app 0.1.0","nodes":[]},"version":1}"#,
        )
        .unwrap();
        // No manifest sits in the project directory, so `resolve.root` names it.
        assert_eq!(graph.root, Some(1));
        let lib = &graph.packages[0];
        assert_eq!(lib.dir(), Path::new("/reg/lib-1.0.0"));
        assert_eq!(lib.source.as_deref(), Some("registry+x"));
        assert!(lib.metadata["frust"]["plugin"].is_object());
        assert_eq!(
            lib.features,
            Some(["default".to_string(), "extra".to_string()].into())
        );
        let app = &graph.packages[1];
        assert_eq!(
            (app.source.clone(), app.metadata.clone()),
            (None, serde_json::Value::Null)
        );
        assert_eq!(app.features, None);
        // A virtual workspace's graph has no root.
        let graph = package_graph_from_metadata(
            Path::new("/work/ws"),
            r#"{"packages":[{"name":"a","version":"1","manifest_path":"/work/ws/a/Cargo.toml"}],
                "resolve":{"root":null,"nodes":[]}}"#,
        )
        .unwrap();
        assert_eq!(graph.root, None);
        assert!(matches!(
            package_graph_from_metadata(Path::new("/p"), r#"{"packages":[{"name":"x"}]}"#),
            Err(PackagesError::MetadataUnreadable { .. })
        ));
    }

    /// The closure follows every edge from the root; the direct set keeps
    /// only the root's normal edges, whatever their target, and drops
    /// dev-only and build-only ones. A package nothing reaches is in neither.
    #[test]
    fn the_package_graph_carries_the_roots_closure_and_direct_dependencies() {
        let package = |name: &str| {
            format!(
                r#"{{"name":"{name}","version":"1.0.0","id":"{name} 1.0.0",
                    "manifest_path":"/reg/{name}/Cargo.toml"}}"#
            )
        };
        let names = [
            "app", "normal", "unix", "dev", "build", "both", "nested", "stray",
        ];
        let packages: Vec<String> = names.iter().map(|name| package(name)).collect();
        let edge = |name: &str, kinds: &str| {
            format!(r#"{{"name":"{name}","pkg":"{name} 1.0.0","dep_kinds":[{kinds}]}}"#)
        };
        let deps = [
            edge("normal", r#"{"kind":null,"target":null}"#),
            edge("unix", r#"{"kind":null,"target":"cfg(unix)"}"#),
            edge("dev", r#"{"kind":"dev","target":null}"#),
            edge("build", r#"{"kind":"build","target":null}"#),
            edge(
                "both",
                r#"{"kind":"dev","target":null},{"kind":null,"target":"cfg(windows)"}"#,
            ),
        ];
        let metadata = format!(
            r#"{{"packages":[{}],"resolve":{{"root":"app 1.0.0","nodes":[
                {{"id":"app 1.0.0","dependencies":["normal 1.0.0","unix 1.0.0","dev 1.0.0",
                  "build 1.0.0","both 1.0.0"],"deps":[{}]}},
                {{"id":"dev 1.0.0","dependencies":["nested 1.0.0"],"deps":[]}},
                {{"id":"stray 1.0.0","dependencies":[],"deps":[]}}
            ]}},"version":1}}"#,
            packages.join(","),
            deps.join(",")
        );
        let graph = package_graph_from_metadata(Path::new("/work/app"), &metadata).unwrap();
        let index = |name: &str| names.iter().position(|n| *n == name).unwrap();
        let set = |members: &[&str]| members.iter().map(|name| index(name)).collect();
        assert_eq!(graph.root, Some(index("app")));
        assert_eq!(
            graph.closure,
            Some(set(&[
                "app", "normal", "unix", "dev", "build", "both", "nested"
            ]))
        );
        assert_eq!(graph.direct, set(&["normal", "unix", "both"]));
        assert!(graph.in_closure(index("nested")) && !graph.in_closure(index("stray")));
        assert!(graph.is_direct(index("unix")) && !graph.is_direct(index("dev")));

        // Without `deps`, the node's plain `dependencies` are the direct set.
        let plain = format!(
            r#"{{"packages":[{},{}],"resolve":{{"root":"app 1.0.0","nodes":[
                {{"id":"app 1.0.0","dependencies":["normal 1.0.0"]}}]}}}}"#,
            package("app"),
            package("normal")
        );
        let graph = package_graph_from_metadata(Path::new("/work/app"), &plain).unwrap();
        assert_eq!(graph.direct, set(&["normal"]));

        // No resolve graph, or no root: no closure and nothing direct, so
        // every package counts as reachable.
        for document in [
            format!(r#"{{"packages":[{}],"resolve":null}}"#, package("normal")),
            format!(
                r#"{{"packages":[{}],"resolve":{{"root":null,"nodes":[]}}}}"#,
                package("normal")
            ),
        ] {
            let graph = package_graph_from_metadata(Path::new("/work/ws"), &document).unwrap();
            assert_eq!((graph.closure.clone(), graph.direct.len()), (None, 0));
            assert!(graph.in_closure(0) && !graph.is_direct(0));
        }
    }

    #[test]
    fn unreadable_metadata_is_a_typed_error() {
        for bad in [
            "not json",
            "{\"version\":1}",
            r#"{"packages":[{"name":"x"}]}"#,
        ] {
            let err = packages_from_metadata(Path::new("/p"), bad, &["x"]).unwrap_err();
            assert!(
                matches!(err, PackagesError::MetadataUnreadable { .. }),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn the_cargo_locator_runs_full_metadata_through_the_runner() {
        let project = project_with("runner", "[package]\nname = \"a\"\n");
        let runner = FakeProcessRunner::new().with(metadata_key(&project), ok(METADATA));
        let dir = CargoLocator::new(&runner)
            .locate(&project, "frust-iap")
            .unwrap();
        assert_eq!(dir, PathBuf::from("/work/frust/plugins/iap"));
        let _ = fs::remove_dir_all(&project);
    }

    /// The default policy resolves as the next build would: no `--locked`,
    /// whether or not a `Cargo.lock` exists. Only the explicit `Locked`
    /// policy passes it.
    #[test]
    fn locked_is_passed_only_under_the_locked_policy() {
        let project = project_with("locked", "[package]\nname = \"a\"\n");
        let manifest = project.join("Cargo.toml");
        let runner = FakeProcessRunner::new();
        let default = CargoLocator::new(&runner);
        for lock in [false, true] {
            if lock {
                fs::write(project.join("Cargo.lock"), "version = 4\n").unwrap();
            }
            let args = default.metadata_args(&manifest);
            assert!(!args.contains(&"--locked".to_string()), "lock={lock}");
            assert!(
                !args.contains(&"--no-deps".to_string()),
                "locating a dependency needs the dependency graph"
            );
        }
        let locked = CargoLocator::new(&runner).with_lockfile_policy(LockfilePolicy::Locked);
        assert_eq!(
            locked.metadata_args(&manifest).last().map(String::as_str),
            Some("--locked")
        );
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn a_missing_cargo_is_cargo_unavailable() {
        let project = project_with("no-cargo", "[package]\nname = \"a\"\n");
        let runner = FakeProcessRunner::new().missing(metadata_key(&project));
        let err = CargoLocator::new(&runner)
            .locate(&project, "frust-ui")
            .unwrap_err();
        assert!(
            matches!(err, PackagesError::CargoUnavailable { .. }),
            "{err:?}"
        );
        let _ = fs::remove_dir_all(&project);
    }

    /// cargo's own stderr is the explanation a user needs (an unreachable
    /// registry, a path dependency that moved), so it is carried verbatim.
    #[test]
    fn a_failed_metadata_run_surfaces_cargos_stderr() {
        let project = project_with("metadata-fails", "[package]\nname = \"a\"\n");
        let runner = FakeProcessRunner::new().with(
            metadata_key(&project),
            Output {
                success: false,
                stdout: String::new(),
                stderr: "error: failed to get `frust-ui` as a dependency of package `a`\n"
                    .to_string(),
            },
        );
        let err = CargoLocator::new(&runner)
            .locate(&project, "frust-ui")
            .unwrap_err();
        let message = err.to_string();
        assert!(
            matches!(err, PackagesError::MetadataFailed { .. }),
            "{err:?}"
        );
        assert!(message.contains("failed to get `frust-ui`"), "{message}");
        assert!(!message.contains("network"), "{message}");
        let _ = fs::remove_dir_all(&project);
    }

    /// A stale lock is cargo's error, not a connectivity problem; an index or
    /// download failure keeps the network hint.
    #[test]
    fn the_network_hint_appears_only_for_network_failures() {
        let project = project_with("hint", "[package]\nname = \"a\"\n");
        let failed = |stderr: &str| PackagesError::MetadataFailed {
            project_dir: project.clone(),
            stderr: stderr.to_string(),
        };
        let stale = failed(
            "error: the lock file needs to be updated but --locked was passed to prevent this",
        )
        .to_string();
        assert!(
            stale.contains("the lock file needs to be updated"),
            "{stale}"
        );
        assert!(!stale.contains("network"), "{stale}");
        for network in [
            "error: failed to download `anyhow v1.0.0`",
            "warning: spurious network error (3 tries remaining)",
            "error: failed to get `x` as a dependency\nCaused by: unable to update registry `crates-io`",
        ] {
            let message = failed(network).to_string();
            assert!(message.contains("network access once"), "{message}");
            assert!(message.contains(network), "{message}");
        }
        let _ = fs::remove_dir_all(&project);
    }

    /// Canned metadata with two `serde` versions: 1.0.1 is a dependency of
    /// the root package `app`, 0.9.0 only of a build-tool package outside the
    /// root's closure.
    fn two_version_metadata(both_reachable: bool) -> String {
        let root_deps = if both_reachable {
            r#""serde 1.0.1", "serde 0.9.0""#
        } else {
            r#""serde 1.0.1""#
        };
        format!(
            r#"{{
            "packages": [
                {{"name":"app","version":"0.1.0","id":"app","manifest_path":"/work/app/Cargo.toml"}},
                {{"name":"tool","version":"0.1.0","id":"tool","manifest_path":"/reg/tool/Cargo.toml"}},
                {{"name":"serde","version":"0.9.0","id":"serde 0.9.0","manifest_path":"/reg/serde-0.9.0/Cargo.toml"}},
                {{"name":"serde","version":"1.0.1","id":"serde 1.0.1","manifest_path":"/reg/serde-1.0.1/Cargo.toml"}}
            ],
            "workspace_members": ["app"],
            "resolve": {{
                "root": "app",
                "nodes": [
                    {{"id":"app","dependencies":[{root_deps}]}},
                    {{"id":"tool","dependencies":["serde 0.9.0"]}},
                    {{"id":"serde 0.9.0","dependencies":[]}},
                    {{"id":"serde 1.0.1","dependencies":[]}}
                ]
            }},
            "version": 1
        }}"#
        )
    }

    #[test]
    fn a_name_shared_by_two_versions_resolves_to_the_roots_dependency() {
        let dirs = packages_from_metadata(
            Path::new("/work/app"),
            &two_version_metadata(false),
            &["serde"],
        )
        .unwrap();
        assert_eq!(dirs, vec![PathBuf::from("/reg/serde-1.0.1")]);
    }

    #[test]
    fn two_versions_both_reachable_from_the_root_are_ambiguous() {
        let err = packages_from_metadata(
            Path::new("/work/app"),
            &two_version_metadata(true),
            &["serde"],
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            matches!(&err, PackagesError::Ambiguous { candidates, .. } if candidates.len() == 2),
            "{err:?}"
        );
        assert!(message.contains("serde@0.9.0"), "{message}");
        assert!(message.contains("serde@1.0.1"), "{message}");
    }

    #[test]
    fn a_directory_without_a_manifest_never_spawns_cargo() {
        let dir = temp_dir("no-manifest");
        let runner = FakeProcessRunner::new();
        let err = CargoLocator::new(&runner)
            .locate(&dir, "frust-ui")
            .unwrap_err();
        assert!(matches!(err, PackagesError::NoManifest { .. }), "{err:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stub_answers_from_its_map_and_reports_absence_like_cargo() {
        let project = project_with(
            "stub",
            "[dependencies]\nfrust = { package = \"frust-ui\", path = \"../frust\" }\n",
        );
        let stub = StubLocator::new().with("frust-shell-web", "/checkout/crates/frust-shell-web");
        assert_eq!(
            stub.locate(&project, "frust-shell-web").unwrap(),
            PathBuf::from("/checkout/crates/frust-shell-web")
        );
        let err = stub.locate(&project, "frust-camera").unwrap_err();
        assert!(err.to_string().contains("path = \"../frust\""), "{err}");
        assert!(matches!(
            StubLocator::failing("offline").locate(&project, "frust-ui"),
            Err(PackagesError::MetadataFailed { .. })
        ));
        let _ = fs::remove_dir_all(&project);
    }

    /// One inner call per distinct name, errors remembered as well as answers.
    #[test]
    fn the_cache_asks_its_inner_locator_once_per_package() {
        let project = temp_dir("cache");
        let stub = StubLocator::new().with("frust-ui", "/f/crates/frust");
        let cached = CachedLocator::new(&stub);
        for _ in 0..3 {
            assert_eq!(
                cached.locate(&project, "frust-ui").unwrap(),
                PathBuf::from("/f/crates/frust")
            );
        }
        assert_eq!(stub.calls(), 1);
        assert!(cached.locate(&project, "frust-camera").is_err());
        assert!(cached.locate(&project, "frust-camera").is_err());
        assert_eq!(stub.calls(), 2);
        assert!(cached.as_graph_locator().is_none());
        assert!(matches!(
            cached.package_graph(&project),
            Err(PackagesError::MetadataUnreadable { .. })
        ));
        assert_eq!(stub.calls(), 2);
        let _ = fs::remove_dir_all(&project);
    }

    /// A [`FakeProcessRunner`] counting the `run` calls that reach it.
    struct Counting {
        inner: FakeProcessRunner,
        runs: std::cell::Cell<usize>,
    }

    impl Counting {
        fn new(inner: FakeProcessRunner) -> Self {
            Self {
                inner,
                runs: std::cell::Cell::new(0),
            }
        }
    }

    impl ProcessRunner for Counting {
        fn run(&self, cmd: &str, args: &[&str]) -> anyhow::Result<Output> {
            self.runs.set(self.runs.get() + 1);
            self.inner.run(cmd, args)
        }

        fn run_streaming(
            &self,
            cmd: &str,
            args: &[&str],
            cwd: Option<&Path>,
            env: &[(&str, &str)],
            on_line: &mut dyn FnMut(&str),
        ) -> anyhow::Result<Output> {
            self.inner.run_streaming(cmd, args, cwd, env, on_line)
        }

        fn spawn_streaming(
            &self,
            cmd: &str,
            args: &[&str],
            cwd: Option<&Path>,
            env: &[(&str, &str)],
        ) -> anyhow::Result<crate::process::StreamHandle> {
            self.inner.spawn_streaming(cmd, args, cwd, env)
        }
    }

    /// Over a [`CargoLocator`], the graph and every package directory of one
    /// project come from a single `cargo metadata` run, answered exactly as
    /// the uncached locator answers.
    #[test]
    fn the_cache_serves_the_graph_and_directories_from_one_metadata_run() {
        let project = project_with("cache-graph", "[package]\nname = \"app\"\n");
        let runner =
            Counting::new(FakeProcessRunner::new().with(metadata_key(&project), ok(METADATA)));
        let cargo = CargoLocator::new(&runner);
        let cached = CachedLocator::new(&cargo);
        assert!(cached.as_graph_locator().is_some());

        let graph = cached.package_graph(&project).unwrap();
        assert_eq!(graph.packages.len(), 3);
        assert_eq!(
            cached
                .locate_many(&project, &["frust-iap", "frust-ui"])
                .unwrap(),
            [
                PathBuf::from("/work/frust/plugins/iap"),
                PathBuf::from("/work/frust/crates/frust")
            ]
        );
        assert!(matches!(
            cached.locate(&project, "frust-camera"),
            Err(PackagesError::PackageAbsent { .. })
        ));
        assert_eq!(cached.package_graph(&project).unwrap(), graph);
        assert_eq!(runner.runs.get(), 1);

        // The uncached locator gives the same answers, one run each.
        assert_eq!(cargo.package_graph(&project).unwrap(), graph);
        assert_eq!(
            cargo.locate(&project, "frust-iap").unwrap(),
            PathBuf::from("/work/frust/plugins/iap")
        );
        assert_eq!(runner.runs.get(), 3);
        let _ = fs::remove_dir_all(&project);
    }

    /// A failed run is remembered as the same typed error, for the graph and
    /// for a directory alike, and cargo is not asked again.
    #[test]
    fn the_cache_returns_the_same_typed_error_for_a_failed_run() {
        let project = project_with("cache-fails", "[package]\nname = \"app\"\n");
        let runner = Counting::new(FakeProcessRunner::new().with(
            metadata_key(&project),
            Output {
                success: false,
                stdout: String::new(),
                stderr: "error: failed to download from the index".to_string(),
            },
        ));
        let cargo = CargoLocator::new(&runner);
        let cached = CachedLocator::new(&cargo);
        let first = cached.package_graph(&project).unwrap_err();
        assert!(
            matches!(&first, PackagesError::MetadataFailed { stderr, .. } if stderr.contains("failed to download")),
            "{first:?}"
        );
        assert_eq!(cached.package_graph(&project).unwrap_err(), first);
        assert_eq!(cached.locate(&project, "frust-ui").unwrap_err(), first);
        assert_eq!(runner.runs.get(), 1);
        let _ = fs::remove_dir_all(&project);
    }

    /// The document answers `Ambiguous` exactly as the uncached parse does.
    #[test]
    fn the_cached_document_keeps_the_ambiguity_error() {
        let project = project_with("cache-ambiguous", "[package]\nname = \"app\"\n");
        let twice = METADATA.replacen(
            r#""packages": ["#,
            r#""packages": [
            {
                "name": "frust-iap",
                "version": "0.4.0",
                "id": "path+file:///old/plugins/iap#frust-iap@0.4.0",
                "source": null,
                "manifest_path": "/old/plugins/iap/Cargo.toml"
            },"#,
            1,
        );
        let runner =
            Counting::new(FakeProcessRunner::new().with(metadata_key(&project), ok(&twice)));
        let cargo = CargoLocator::new(&runner);
        let cached = CachedLocator::new(&cargo);
        let expected = packages_from_metadata(&project, &twice, &["frust-iap"]).unwrap_err();
        assert!(
            matches!(expected, PackagesError::Ambiguous { .. }),
            "{expected:?}"
        );
        assert_eq!(cached.locate(&project, "frust-iap").unwrap_err(), expected);
        assert!(cached.package_graph(&project).is_ok());
        assert_eq!(runner.runs.get(), 1);
        let _ = fs::remove_dir_all(&project);
    }

    /// The workspace root of this checkout (`crates/frust-drive/../..`).
    fn checkout_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    /// End to end against real cargo: a path-mode project generated with
    /// `--frust-path` = this checkout resolves `frust-shell-web` to the
    /// checkout's own crate directory. The checkout's `Cargo.lock` seeds the
    /// generated project's, so cargo resolves the framework graph from the
    /// versions the workspace already pins rather than fetching an index.
    /// Ignored with the e2e family: it spawns real cargo and writes a project
    /// under the temp dir; run it with `--ignored`.
    #[test]
    #[ignore = "spawns real cargo against a generated project; run with --ignored (e2e family)"]
    fn real_cargo_locates_the_shell_crate_of_a_generated_path_mode_project() {
        let root = checkout_root();
        let project = temp_dir("real-cargo").join("my_app");
        let ctx = TemplateContext {
            project_name: "my_app".into(),
            title_case_name: "My App".into(),
            org: "dev.f0x".into(),
            description: "A new Frust application.".into(),
            frust_version: env!("CARGO_PKG_VERSION").into(),
            frust: FrustDependency::Path(root.join("crates/frust").to_string_lossy().into_owned()),
            deeplink_scheme: None,
            deeplink_host: None,
        };
        scaffold::generate(&project, &ctx, None, false, None).unwrap();
        fs::copy(root.join("Cargo.lock"), project.join("Cargo.lock")).unwrap();

        let located = CargoLocator::new(&RealProcessRunner)
            .locate(&project, "frust-ui")
            .unwrap();
        assert_eq!(located.canonicalize().unwrap(), root.join("crates/frust"));

        let dirs = locate_many(&project, &["frust-shell-web", "frust-glyph"]).unwrap();
        assert_eq!(
            dirs.iter()
                .map(|dir| dir.canonicalize().unwrap())
                .collect::<Vec<_>>(),
            vec![
                root.join("crates/frust-shell-web"),
                root.join("plugins/glyph")
            ]
        );
        assert!(
            dirs[0].join("platform/web/index.html").is_file(),
            "the located crate carries the browser host page"
        );
        let _ = fs::remove_dir_all(project.parent().unwrap());
    }
}
