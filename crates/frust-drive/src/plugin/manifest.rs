//! Plugins described by a package's own Cargo metadata; [`discover`] finds
//! every such package in an app's resolved dependency graph. The schema
//! (any other key is an error; `path`s are relative to the package):
//!
//! ```toml
//! [package.metadata.frust.plugin]
//! id = "acme-ble"                # required, kebab-case, unique in the app
//! summary = "Acme BLE scanner"   # required, one line
//! min-ios = "16.4"               # optional, dotted integers
//! min-android-sdk = 31           # optional
//! [package.metadata.frust.plugin.android]
//! gradle-module = { name = "frust-acme-ble", path = "platform/android" }
//! permissions = ["android.permission.BLUETOOTH_CONNECT"]
//! maven-repositories = [{ url = "https://maven.example.com", groups = ["com.example.acme"] }]
//! [package.metadata.frust.plugin.ios]
//! swift-package = { name = "FrustAcmeBle", path = "platform/ios" }
//! frameworks = ["CoreBluetooth"]
//! plist = { NSBluetoothAlwaysUsageDescription = "Finds nearby Acme devices." }
//! plist-arrays = { "com.example.key" = ["a", "b"] }
//! entitlements = { "com.example.flag" = true, "com.example.mode" = "development" }
//! [package.metadata.frust.plugin.features.<name>]  # summary, min-ios, min-android-sdk,
//! cargo-feature = "<feature of this crate>"        # android, ios: all optional
//! ```
//! The base starts with the package's `CargoDep`; each entry maps to its [`Contribution`].

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::catalog::{CatalogEntry, DepSource, PluginOrigin};
use super::{Contribution, FeatureSpec, IosEntitlementValue, PluginSpec};
use crate::packages::{CargoLocator, GraphPackage, PackageGraph, PackagesError};

/// The table a package describes itself as a plugin in.
const ROOT_KEY: &str = "package.metadata.frust.plugin";

/// Why one package's plugin description was refused; that package is skipped
/// whole, never applied in part. `key` is the dotted key at fault.
///
/// Checked: the id is kebab-case and the summary one non-empty line; a `path`
/// is relative, has no `..`, and names a directory inside the package; a Maven
/// url is http(s) with at least one group; `min-ios` is dotted integers;
/// permission and framework names match `[A-Za-z0-9_.]+`; Gradle module and
/// Swift package names match `[A-Za-z0-9_-]+`; plist and entitlement keys are
/// non-empty; an entitlement is a bool, a string or an array of strings; a
/// `cargo-feature` is one of the crate's features; and no key is unknown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plugin manifest of package `{package}`: `{key}` {reason}")]
pub struct ManifestError {
    /// The cargo package whose metadata is at fault.
    pub package: String,
    /// The dotted key at fault, from `package.metadata.frust.plugin`.
    pub key: String,
    /// What is wrong with it.
    pub reason: String,
}

/// What [`discover`] found: the plugins that parsed, ordered by package name,
/// and one [`ManifestError`] per package that did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// One [`PluginOrigin::Manifest`] entry per valid plugin package.
    pub plugins: Vec<CatalogEntry>,
    /// The packages skipped, and why.
    pub problems: Vec<ManifestError>,
}

/// Every package in the resolved dependency graph of the app at `project_dir`
/// that describes itself as a plugin, from one `cargo metadata` run through
/// `locator`. The app's own root package is never a plugin. Of two packages
/// declaring the same id the first by package name is kept and the other is
/// reported in [`Discovery::problems`]. In a discovered spec, `crate_dir` is
/// the package name and a native module's `rel_path` is relative to the
/// package directory.
pub fn discover(
    project_dir: &Path,
    locator: &CargoLocator<'_>,
) -> Result<Discovery, PackagesError> {
    Ok(discover_in(&locator.package_graph(project_dir)?))
}

/// [`discover`] over an already-read package graph.
pub(crate) fn discover_in(graph: &PackageGraph) -> Discovery {
    let mut candidates: Vec<(&GraphPackage, &Value)> = graph
        .packages
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != graph.root)
        .filter_map(|(_, package)| {
            let table = package.metadata.get("frust")?.get("plugin")?;
            Some((package, table))
        })
        .collect();
    candidates.sort_by(|(a, _), (b, _)| (&a.name, &a.version).cmp(&(&b.name, &b.version)));

    let mut discovery = Discovery::default();
    for (package, table) in candidates {
        match plugin_entry(package, table) {
            Ok(entry) => {
                let earlier = discovery
                    .plugins
                    .iter()
                    .find(|e| e.spec.id == entry.spec.id);
                if let Some(earlier) = earlier {
                    let earlier = match &earlier.origin {
                        PluginOrigin::Manifest { package, .. } => package.as_str(),
                        PluginOrigin::BuiltIn => "a built-in plugin",
                    };
                    discovery.problems.push(ManifestError {
                        package: package.name.clone(),
                        key: format!("{ROOT_KEY}.id"),
                        reason: format!(
                            "is `{}`, which package `{earlier}` already declares",
                            entry.spec.id
                        ),
                    });
                } else {
                    discovery.plugins.push(entry);
                }
            }
            Err(err) => discovery.problems.push(err),
        }
    }
    discovery
}

/// One package's catalog entry, from its `package.metadata.frust.plugin`
/// value.
pub(crate) fn plugin_entry(
    package: &GraphPackage,
    table: &Value,
) -> Result<CatalogEntry, ManifestError> {
    let ctx = Ctx {
        package: &package.name,
        package_dir: package.dir(),
        features: package.features.as_ref(),
    };
    Ok(CatalogEntry {
        spec: plugin_spec(&ctx, table)?,
        origin: PluginOrigin::Manifest {
            package: package.name.clone(),
            package_dir: package.dir().to_path_buf(),
            version: package.version.clone(),
            source: DepSource::from_cargo_source(package.source.as_deref()),
        },
    })
}

/// The package a description is read for.
struct Ctx<'a> {
    package: &'a str,
    package_dir: &'a Path,
    /// The crate's own feature names, when cargo listed them.
    features: Option<&'a BTreeSet<String>>,
}

impl Ctx<'_> {
    fn err(&self, key: &str, reason: impl Into<String>) -> ManifestError {
        ManifestError {
            package: self.package.to_string(),
            key: key.to_string(),
            reason: reason.into(),
        }
    }

    /// `value` as `T`, a serde failure (an unknown or missing key, a wrong
    /// type) reported against `key`.
    fn de<T: DeserializeOwned>(&self, key: &str, value: &Value) -> Result<T, ManifestError> {
        T::deserialize(value).map_err(|err| self.err(key, format!("is invalid: {err}")))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct PluginTable {
    id: String,
    summary: String,
    #[serde(default)]
    min_ios: Option<String>,
    #[serde(default)]
    min_android_sdk: Option<u32>,
    #[serde(default)]
    android: Option<Value>,
    #[serde(default)]
    ios: Option<Value>,
    #[serde(default)]
    features: Option<BTreeMap<String, Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FeatureTable {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    cargo_feature: Option<String>,
    #[serde(default)]
    min_ios: Option<String>,
    #[serde(default)]
    min_android_sdk: Option<u32>,
    #[serde(default)]
    android: Option<Value>,
    #[serde(default)]
    ios: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct AndroidTable {
    #[serde(default)]
    gradle_module: Option<ModuleRef>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    maven_repositories: Vec<MavenRepository>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct IosTable {
    #[serde(default)]
    swift_package: Option<ModuleRef>,
    #[serde(default)]
    frameworks: Vec<String>,
    #[serde(default)]
    plist: BTreeMap<String, String>,
    #[serde(default)]
    plist_arrays: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    entitlements: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleRef {
    name: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MavenRepository {
    url: String,
    groups: Vec<String>,
}

fn plugin_spec(ctx: &Ctx<'_>, table: &Value) -> Result<PluginSpec, ManifestError> {
    if !table.is_object() {
        return Err(ctx.err(ROOT_KEY, "must be a table"));
    }
    let plugin: PluginTable = ctx.de(ROOT_KEY, table)?;
    if !is_kebab(&plugin.id) {
        return Err(ctx.err(
            &format!("{ROOT_KEY}.id"),
            format!(
                "is `{}`; an id is kebab-case: lowercase letters and digits in words joined by single `-`",
                plugin.id
            ),
        ));
    }
    let summary_key = format!("{ROOT_KEY}.summary");
    if plugin.summary.trim().is_empty() || plugin.summary.contains(['\n', '\r']) {
        return Err(ctx.err(&summary_key, "must be one non-empty line"));
    }
    let min_ios = ios_floor(ctx, ROOT_KEY, plugin.min_ios)?;
    // The id is kebab-case, so the comment cannot carry the `--` XML forbids.
    let comment = format!("Added by plugin {}", plugin.id);

    let mut base = vec![Contribution::CargoDep {
        name: owned(ctx.package),
    }];
    platform(
        ctx,
        ROOT_KEY,
        plugin.android,
        plugin.ios,
        &comment,
        &mut base,
    )?;

    let mut features = Vec::new();
    for (name, value) in plugin.features.unwrap_or_default() {
        let key = format!("{ROOT_KEY}.features.{name}");
        if !is_kebab(&name) {
            return Err(ctx.err(&key, "names a feature that is not kebab-case"));
        }
        let feature: FeatureTable = ctx.de(&key, &value)?;
        let summary = feature.summary.unwrap_or_default();
        if summary.contains(['\n', '\r']) {
            return Err(ctx.err(&format!("{key}.summary"), "must be one line"));
        }
        let mut contributions = Vec::new();
        if let Some(cargo_feature) = feature.cargo_feature {
            let cargo_key = format!("{key}.cargo-feature");
            if !matches_charset(&cargo_feature, |c| {
                c.is_ascii_alphanumeric() || "_-+.".contains(c)
            }) {
                return Err(ctx.err(&cargo_key, "is not a cargo feature name"));
            }
            if ctx
                .features
                .is_some_and(|known| !known.contains(&cargo_feature))
            {
                return Err(ctx.err(
                    &cargo_key,
                    format!("names `{cargo_feature}`, which is not a feature of this crate"),
                ));
            }
            contributions.push(Contribution::CargoFeature {
                name: owned(ctx.package),
                feature: Cow::Owned(cargo_feature),
            });
        }
        platform(
            ctx,
            &key,
            feature.android,
            feature.ios,
            &comment,
            &mut contributions,
        )?;
        features.push(FeatureSpec {
            id: Cow::Owned(name),
            summary: Cow::Owned(summary),
            contributions: Cow::Owned(contributions),
            min_ios: ios_floor(ctx, &key, feature.min_ios)?,
            min_android_sdk: feature.min_android_sdk,
        });
    }

    Ok(PluginSpec {
        id: Cow::Owned(plugin.id),
        summary: Cow::Owned(plugin.summary),
        crate_dir: owned(ctx.package),
        base: Cow::Owned(base),
        optional_features: Cow::Owned(features),
        requires_sibling: None,
        min_ios,
        min_android_sdk: plugin.min_android_sdk,
    })
}

/// Appends the contributions of the `android` and `ios` tables under
/// `prefix`, Android first.
fn platform(
    ctx: &Ctx<'_>,
    prefix: &str,
    android: Option<Value>,
    ios: Option<Value>,
    comment: &str,
    out: &mut Vec<Contribution>,
) -> Result<(), ManifestError> {
    if let Some(android) = android {
        let key = format!("{prefix}.android");
        let table: AndroidTable = ctx.de(&key, &android)?;
        if let Some(module) = table.gradle_module {
            let module_key = format!("{key}.gradle-module");
            let name = module_name(ctx, &module_key, module.name)?;
            out.push(Contribution::GradleModule {
                gradle_name: Cow::Owned(format!(":{name}")),
                rel_path: Cow::Owned(package_path(ctx, &module_key, &module.path)?),
            });
        }
        for permission in table.permissions {
            let permission = dotted_name(ctx, &format!("{key}.permissions"), permission)?;
            out.push(Contribution::ManifestPermission {
                permission: Cow::Owned(permission),
            });
        }
        for repository in table.maven_repositories {
            let repo_key = format!("{key}.maven-repositories");
            let rest = repository
                .url
                .strip_prefix("https://")
                .or_else(|| repository.url.strip_prefix("http://"));
            if rest.is_none_or(|rest| rest.is_empty() || rest.contains(char::is_whitespace)) {
                return Err(ctx.err(
                    &repo_key,
                    format!("has url `{}`; it must be an http(s) url", repository.url),
                ));
            }
            if repository.groups.is_empty() {
                return Err(ctx.err(
                    &repo_key,
                    format!(
                        "has no groups for `{}`; list the Maven groups it serves",
                        repository.url
                    ),
                ));
            }
            for group in &repository.groups {
                if !matches_charset(group, |c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
                    return Err(ctx.err(&repo_key, format!("has an invalid group `{group}`")));
                }
            }
            out.push(Contribution::MavenRepository {
                url: Cow::Owned(repository.url),
                groups: owned_list(repository.groups),
                comment: owned(comment),
            });
        }
    }
    if let Some(ios) = ios {
        let key = format!("{prefix}.ios");
        let table: IosTable = ctx.de(&key, &ios)?;
        if let Some(package) = table.swift_package {
            let package_key = format!("{key}.swift-package");
            out.push(Contribution::SwiftPackageRef {
                package_name: Cow::Owned(module_name(ctx, &package_key, package.name)?),
                rel_path: Cow::Owned(package_path(ctx, &package_key, &package.path)?),
            });
        }
        for framework in table.frameworks {
            let name = dotted_name(ctx, &format!("{key}.frameworks"), framework)?;
            out.push(Contribution::IosFramework {
                name: Cow::Owned(name),
            });
        }
        for (plist_key, value) in table.plist {
            non_empty_key(ctx, &format!("{key}.plist"), &plist_key)?;
            out.push(Contribution::PlistEntry {
                key: Cow::Owned(plist_key),
                value: Cow::Owned(value),
                comment: owned(comment),
            });
        }
        for (plist_key, values) in table.plist_arrays {
            non_empty_key(ctx, &format!("{key}.plist-arrays"), &plist_key)?;
            out.push(Contribution::PlistArrayEntry {
                key: Cow::Owned(plist_key),
                values: owned_list(values),
                comment: owned(comment),
            });
        }
        for (entitlement, value) in table.entitlements {
            let entitlements_key = format!("{key}.entitlements");
            non_empty_key(ctx, &entitlements_key, &entitlement)?;
            let value = entitlement_value(&value).ok_or_else(|| {
                ctx.err(
                    &format!("{entitlements_key}.{entitlement}"),
                    "must be a bool, a string or an array of strings",
                )
            })?;
            out.push(Contribution::IosEntitlement {
                key: Cow::Owned(entitlement),
                value,
                comment: owned(comment),
            });
        }
    }
    Ok(())
}

fn entitlement_value(value: &Value) -> Option<IosEntitlementValue> {
    match value {
        Value::Bool(flag) => Some(IosEntitlementValue::Bool(*flag)),
        Value::String(text) => Some(IosEntitlementValue::String(Cow::Owned(text.clone()))),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(|s| Cow::Owned(s.to_string())))
            .collect::<Option<Vec<_>>>()
            .map(|items| IosEntitlementValue::StringArray(Cow::Owned(items))),
        _ => None,
    }
}

/// A `path` value as a `/`-joined path inside the package directory, which
/// must exist there as a directory (symlinks resolved).
fn package_path(ctx: &Ctx<'_>, key: &str, raw: &str) -> Result<String, ManifestError> {
    let key = format!("{key}.path");
    let refuse = |reason: &str| Err(ctx.err(&key, format!("is `{raw}`; {reason}")));
    if raw.contains('\\') {
        return refuse("separate its components with `/`");
    }
    let path = Path::new(raw);
    if path.is_absolute() || raw.starts_with('/') {
        return refuse("it must be relative to the package directory");
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part),
                None => return refuse("it is not valid UTF-8"),
            },
            Component::CurDir => {}
            Component::ParentDir => return refuse("it must not contain a `..` component"),
            Component::RootDir | Component::Prefix(_) => {
                return refuse("it must be relative to the package directory");
            }
        }
    }
    if parts.is_empty() {
        return refuse("it must name a directory inside the package");
    }
    let rel = parts.join("/");
    let dir = ctx.package_dir.join(&rel);
    if !dir.is_dir() {
        return refuse("no such directory exists in the package");
    }
    let inside = match (dir.canonicalize(), ctx.package_dir.canonicalize()) {
        (Ok(dir), Ok(base)) => dir.starts_with(base),
        _ => false,
    };
    if !inside {
        return refuse("it resolves outside the package directory");
    }
    Ok(rel)
}

fn ios_floor(
    ctx: &Ctx<'_>,
    prefix: &str,
    value: Option<String>,
) -> Result<Option<Cow<'static, str>>, ManifestError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let dotted = value
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if !dotted {
        return Err(ctx.err(
            &format!("{prefix}.min-ios"),
            format!("is `{value}`; it must be dot-separated numbers such as `16.4`"),
        ));
    }
    Ok(Some(Cow::Owned(value)))
}

/// A permission or framework name: `[A-Za-z0-9_.]+`.
fn dotted_name(ctx: &Ctx<'_>, key: &str, name: String) -> Result<String, ManifestError> {
    if matches_charset(&name, |c| c.is_ascii_alphanumeric() || "_.".contains(c)) {
        Ok(name)
    } else {
        Err(ctx.err(
            key,
            format!("has `{name}`, which is not of `[A-Za-z0-9_.]+`"),
        ))
    }
}

/// A Gradle module or Swift package name: `[A-Za-z0-9_-]+`, not starting
/// with `-`.
fn module_name(ctx: &Ctx<'_>, key: &str, name: String) -> Result<String, ManifestError> {
    if matches_charset(&name, |c| c.is_ascii_alphanumeric() || "_-".contains(c))
        && !name.starts_with('-')
    {
        Ok(name)
    } else {
        Err(ctx.err(
            &format!("{key}.name"),
            format!("is `{name}`; it must be of `[A-Za-z0-9_-]+` and not start with `-`"),
        ))
    }
}

fn non_empty_key(ctx: &Ctx<'_>, key: &str, entry: &str) -> Result<(), ManifestError> {
    if entry.trim().is_empty() {
        Err(ctx.err(key, "has an empty key"))
    } else {
        Ok(())
    }
}

fn is_kebab(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn matches_charset(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.chars().all(allowed)
}

fn owned(text: &str) -> Cow<'static, str> {
    Cow::Owned(text.to_string())
}

fn owned_list(items: Vec<String>) -> Cow<'static, [Cow<'static, str>]> {
    Cow::Owned(items.into_iter().map(Cow::Owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeProcessRunner, Output};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "frust-drive-plugin-manifest-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A package directory carrying both native module directories.
    fn package_dir(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        fs::create_dir_all(dir.join("platform/android")).unwrap();
        fs::create_dir_all(dir.join("platform/ios")).unwrap();
        dir
    }

    /// `[package.metadata]` of a `Cargo.toml` text, as cargo metadata's JSON.
    fn metadata_of(cargo_toml: &str) -> Value {
        let doc: toml::Table = toml::from_str(cargo_toml).unwrap();
        let json = serde_json::to_value(doc).unwrap();
        json["package"]["metadata"].clone()
    }

    fn package(name: &str, dir: &Path, cargo_toml: &str) -> GraphPackage {
        GraphPackage {
            name: name.to_string(),
            version: "0.3.1".to_string(),
            source: None,
            manifest_path: dir.join("Cargo.toml"),
            metadata: metadata_of(cargo_toml),
            features: Some(["scan".to_string(), "default".to_string()].into()),
        }
    }

    fn parse(dir: &Path, cargo_toml: &str) -> Result<CatalogEntry, ManifestError> {
        let package = package("frust-acme-ble", dir, cargo_toml);
        plugin_entry(&package, &package.metadata["frust"]["plugin"])
    }

    const FULL: &str = r#"
        [package.metadata.frust.plugin]
        id = "acme-ble"
        summary = "Acme BLE scanner"
        min-ios = "16.4"
        min-android-sdk = 31

        [package.metadata.frust.plugin.android]
        gradle-module = { name = "frust-acme-ble", path = "./platform/android" }
        permissions = ["android.permission.BLUETOOTH_CONNECT", "android.permission.BLUETOOTH_SCAN"]
        maven-repositories = [{ url = "https://maven.example.com/releases", groups = ["com.example.acme", "com.example.radio"] }]

        [package.metadata.frust.plugin.ios]
        swift-package = { name = "FrustAcmeBle", path = "platform/ios" }
        frameworks = ["AcmeRadio"]
        plist = { NSBluetoothAlwaysUsageDescription = "Finds nearby Acme devices.", AcmeMode = "on" }
        plist-arrays = { "com.example.key" = ["a", "b"] }
        entitlements = { "com.example.formats" = ["TAG"], "com.example.flag" = true, "com.example.mode" = "development" }

        [package.metadata.frust.plugin.features.scan]
        summary = "Background scanning"
        cargo-feature = "scan"
        min-ios = "17"
        min-android-sdk = 33
        android = { permissions = ["android.permission.ACCESS_FINE_LOCATION"] }
        ios = { plist = { NSLocationUsageDescription = "Scans in the background." } }

        [package.metadata.frust.plugin.features.logging]
    "#;

    fn s(text: &'static str) -> Cow<'static, str> {
        Cow::Borrowed(text)
    }

    #[test]
    fn a_full_manifest_maps_every_entry_to_its_contribution() {
        let dir = package_dir("full");
        let entry = parse(&dir, FULL).unwrap();
        let comment = s("Added by plugin acme-ble");
        let expected = PluginSpec {
            id: s("acme-ble"),
            summary: s("Acme BLE scanner"),
            crate_dir: s("frust-acme-ble"),
            base: Cow::Owned(vec![
                Contribution::CargoDep {
                    name: s("frust-acme-ble"),
                },
                Contribution::GradleModule {
                    gradle_name: s(":frust-acme-ble"),
                    rel_path: s("platform/android"),
                },
                Contribution::ManifestPermission {
                    permission: s("android.permission.BLUETOOTH_CONNECT"),
                },
                Contribution::ManifestPermission {
                    permission: s("android.permission.BLUETOOTH_SCAN"),
                },
                Contribution::MavenRepository {
                    url: s("https://maven.example.com/releases"),
                    groups: Cow::Owned(vec![s("com.example.acme"), s("com.example.radio")]),
                    comment: comment.clone(),
                },
                Contribution::SwiftPackageRef {
                    package_name: s("FrustAcmeBle"),
                    rel_path: s("platform/ios"),
                },
                Contribution::IosFramework {
                    name: s("AcmeRadio"),
                },
                Contribution::PlistEntry {
                    key: s("AcmeMode"),
                    value: s("on"),
                    comment: comment.clone(),
                },
                Contribution::PlistEntry {
                    key: s("NSBluetoothAlwaysUsageDescription"),
                    value: s("Finds nearby Acme devices."),
                    comment: comment.clone(),
                },
                Contribution::PlistArrayEntry {
                    key: s("com.example.key"),
                    values: Cow::Owned(vec![s("a"), s("b")]),
                    comment: comment.clone(),
                },
                Contribution::IosEntitlement {
                    key: s("com.example.flag"),
                    value: IosEntitlementValue::Bool(true),
                    comment: comment.clone(),
                },
                Contribution::IosEntitlement {
                    key: s("com.example.formats"),
                    value: IosEntitlementValue::StringArray(Cow::Owned(vec![s("TAG")])),
                    comment: comment.clone(),
                },
                Contribution::IosEntitlement {
                    key: s("com.example.mode"),
                    value: IosEntitlementValue::String(s("development")),
                    comment: comment.clone(),
                },
            ]),
            optional_features: Cow::Owned(vec![
                FeatureSpec {
                    id: s("logging"),
                    ..FeatureSpec::DEFAULT
                },
                FeatureSpec {
                    id: s("scan"),
                    summary: s("Background scanning"),
                    contributions: Cow::Owned(vec![
                        Contribution::CargoFeature {
                            name: s("frust-acme-ble"),
                            feature: s("scan"),
                        },
                        Contribution::ManifestPermission {
                            permission: s("android.permission.ACCESS_FINE_LOCATION"),
                        },
                        Contribution::PlistEntry {
                            key: s("NSLocationUsageDescription"),
                            value: s("Scans in the background."),
                            comment: comment.clone(),
                        },
                    ]),
                    min_ios: Some(s("17")),
                    min_android_sdk: Some(33),
                },
            ]),
            requires_sibling: None,
            min_ios: Some(s("16.4")),
            min_android_sdk: Some(31),
        };
        assert_eq!(entry.spec, expected);
        assert_eq!(
            entry.origin,
            PluginOrigin::Manifest {
                package: "frust-acme-ble".to_string(),
                package_dir: dir.clone(),
                version: "0.3.1".to_string(),
                source: DepSource::Path,
            }
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_minimal_manifest_is_the_cargo_dependency_alone() {
        let dir = temp_dir("minimal");
        let entry = parse(
            &dir,
            "[package.metadata.frust.plugin]\nid = \"acme-ble\"\nsummary = \"Acme\"\n",
        )
        .unwrap();
        assert_eq!(
            &*entry.spec.base,
            [Contribution::CargoDep {
                name: s("frust-acme-ble")
            }]
        );
        assert!(entry.spec.optional_features.is_empty());
        assert_eq!(
            (entry.spec.min_ios, entry.spec.min_android_sdk),
            (None, None)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// `body` is appended to a valid plugin table; the parse must fail with
    /// `key` (after `package.metadata.frust.plugin.`) at fault.
    fn assert_rejected(tag: &str, body: &str, key: &str) {
        let dir = package_dir(tag);
        let header = "[package.metadata.frust.plugin]\nsummary = \"Acme\"\n";
        let id = if body.contains("\nid =") || body.starts_with("id =") {
            ""
        } else {
            "id = \"acme-ble\"\n"
        };
        let err = parse(&dir, &format!("{header}{id}{body}")).unwrap_err();
        assert_eq!(err.package, "frust-acme-ble", "{tag}");
        assert_eq!(err.key, format!("{ROOT_KEY}{key}"), "{tag}: {err}");
        assert!(err.to_string().contains("frust-acme-ble"), "{tag}: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_id_must_be_kebab_case() {
        for (tag, id) in [
            ("upper", "Acme-ble"),
            ("underscore", "acme_ble"),
            ("double", "acme--ble"),
            ("leading", "-acme"),
            ("trailing", "acme-"),
            ("empty", ""),
        ] {
            assert_rejected(tag, &format!("id = \"{id}\"\n"), ".id");
        }
    }

    #[test]
    fn the_summary_must_be_one_non_empty_line() {
        for (tag, summary) in [("blank", "  "), ("two-lines", "one\\ntwo")] {
            let dir = temp_dir(tag);
            let err = parse(
                &dir,
                &format!(
                    "[package.metadata.frust.plugin]\nid = \"acme-ble\"\nsummary = \"{summary}\"\n"
                ),
            )
            .unwrap_err();
            assert_eq!(err.key, format!("{ROOT_KEY}.summary"), "{tag}");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_missing_required_key_is_refused() {
        let dir = temp_dir("no-summary");
        let err = parse(&dir, "[package.metadata.frust.plugin]\nid = \"acme-ble\"\n").unwrap_err();
        assert_eq!(err.key, ROOT_KEY);
        assert!(err.reason.contains("summary"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_keys_are_refused_at_every_level() {
        for (tag, body, key) in [
            ("top", "summry = \"x\"\n", ""),
            (
                "android",
                "[package.metadata.frust.plugin.android]\npermisions = []\n",
                ".android",
            ),
            (
                "ios",
                "[package.metadata.frust.plugin.ios]\nframework = [\"AcmeRadio\"]\n",
                ".ios",
            ),
            (
                "module",
                "[package.metadata.frust.plugin.ios]\nswift-package = { name = \"A\", path = \"platform/ios\", kind = \"x\" }\n",
                ".ios",
            ),
            (
                "desktop",
                "[package.metadata.frust.plugin.macos]\nentitlements = []\n",
                "",
            ),
            (
                "feature",
                "[package.metadata.frust.plugin.features.scan]\ncargo_feature = \"scan\"\n",
                ".features.scan",
            ),
        ] {
            assert_rejected(tag, body, key);
        }
    }

    #[test]
    fn a_path_must_stay_inside_the_package() {
        for (tag, path) in [
            ("absolute", "/etc"),
            ("parent", "platform/../../elsewhere"),
            ("missing", "platform/web"),
            ("package-root", "."),
            ("backslash", "platform\\\\android"),
        ] {
            assert_rejected(
                tag,
                &format!(
                    "[package.metadata.frust.plugin.android]\ngradle-module = {{ name = \"m\", path = \"{path}\" }}\n"
                ),
                ".android.gradle-module.path",
            );
            assert_rejected(
                tag,
                &format!(
                    "[package.metadata.frust.plugin.ios]\nswift-package = {{ name = \"M\", path = \"{path}\" }}\n"
                ),
                ".ios.swift-package.path",
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_symlinked_out_of_the_package_is_refused() {
        let dir = package_dir("symlink");
        let outside = temp_dir("symlink-target");
        std::os::unix::fs::symlink(&outside, dir.join("platform/escape")).unwrap();
        let err = parse(
            &dir,
            "[package.metadata.frust.plugin]\nid = \"acme-ble\"\nsummary = \"Acme\"\n\
             [package.metadata.frust.plugin.android]\n\
             gradle-module = { name = \"m\", path = \"platform/escape\" }\n",
        )
        .unwrap_err();
        assert!(err.reason.contains("outside the package"), "{err}");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn module_names_are_checked() {
        for name in ["", ":frust-acme", "-acme", "acme ble", "a/b"] {
            assert_rejected(
                "gradle-name",
                &format!(
                    "[package.metadata.frust.plugin.android]\ngradle-module = {{ name = \"{name}\", path = \"platform/android\" }}\n"
                ),
                ".android.gradle-module.name",
            );
        }
    }

    #[test]
    fn a_maven_repository_needs_an_http_url_and_groups() {
        for (tag, repository) in [
            (
                "ftp",
                r#"{ url = "ftp://maven.example.com", groups = ["com.example"] }"#,
            ),
            ("bare", r#"{ url = "https://", groups = ["com.example"] }"#),
            (
                "no-scheme",
                r#"{ url = "maven.example.com", groups = ["com.example"] }"#,
            ),
            (
                "no-groups",
                r#"{ url = "https://maven.example.com", groups = [] }"#,
            ),
            (
                "bad-group",
                r#"{ url = "https://maven.example.com", groups = ["com example"] }"#,
            ),
        ] {
            assert_rejected(
                tag,
                &format!(
                    "[package.metadata.frust.plugin.android]\nmaven-repositories = [{repository}]\n"
                ),
                ".android.maven-repositories",
            );
        }
        // A repository with its groups left out entirely is a serde refusal.
        assert_rejected(
            "groups-absent",
            "[package.metadata.frust.plugin.android]\nmaven-repositories = [{ url = \"https://maven.example.com\" }]\n",
            ".android",
        );
    }

    #[test]
    fn min_ios_must_be_dotted_integers() {
        for value in ["16.x", "16.", ".4", "", "v16"] {
            assert_rejected("min-ios", &format!("min-ios = \"{value}\"\n"), ".min-ios");
            assert_rejected(
                "feature-min-ios",
                &format!("[package.metadata.frust.plugin.features.scan]\nmin-ios = \"{value}\"\n"),
                ".features.scan.min-ios",
            );
        }
        // A number rather than a string is a type error on the table.
        assert_rejected("min-ios-float", "min-ios = 16.4\n", "");
    }

    #[test]
    fn permission_and_framework_names_are_checked() {
        for name in ["android permission", "a-b", "", "x/y"] {
            assert_rejected(
                "permission",
                &format!("[package.metadata.frust.plugin.android]\npermissions = [\"{name}\"]\n"),
                ".android.permissions",
            );
            assert_rejected(
                "framework",
                &format!("[package.metadata.frust.plugin.ios]\nframeworks = [\"{name}\"]\n"),
                ".ios.frameworks",
            );
        }
    }

    #[test]
    fn an_entitlement_is_a_bool_a_string_or_a_string_array() {
        for (tag, value) in [
            ("number", "3"),
            ("mixed", "[\"a\", 1]"),
            ("table", "{ a = 1 }"),
        ] {
            assert_rejected(
                tag,
                &format!(
                    "[package.metadata.frust.plugin.ios]\nentitlements = {{ \"com.example.flag\" = {value} }}\n"
                ),
                ".ios.entitlements.com.example.flag",
            );
        }
        assert_rejected(
            "empty-key",
            "[package.metadata.frust.plugin.ios]\nentitlements = { \"\" = true }\n",
            ".ios.entitlements",
        );
    }

    #[test]
    fn plist_keys_must_not_be_empty() {
        assert_rejected(
            "plist",
            "[package.metadata.frust.plugin.ios]\nplist = { \" \" = \"x\" }\n",
            ".ios.plist",
        );
        assert_rejected(
            "plist-arrays",
            "[package.metadata.frust.plugin.ios]\nplist-arrays = { \"\" = [] }\n",
            ".ios.plist-arrays",
        );
        // Values are strings, so a non-string is a type error on the table.
        assert_rejected(
            "plist-value",
            "[package.metadata.frust.plugin.ios]\nplist = { AcmeMode = true }\n",
            ".ios",
        );
    }

    #[test]
    fn a_feature_is_kebab_case_and_names_a_feature_of_the_crate() {
        assert_rejected(
            "feature-name",
            "[package.metadata.frust.plugin.features.Scan]\n",
            ".features.Scan",
        );
        assert_rejected(
            "unknown-cargo-feature",
            "[package.metadata.frust.plugin.features.scan]\ncargo-feature = \"turbo\"\n",
            ".features.scan.cargo-feature",
        );
        assert_rejected(
            "feature-summary",
            "[package.metadata.frust.plugin.features.scan]\nsummary = \"a\\nb\"\n",
            ".features.scan.summary",
        );
    }

    #[test]
    fn a_git_package_is_neither_a_path_nor_a_registry_dependency() {
        let dir = package_dir("git-source");
        let mut package = package("frust-acme-ble", &dir, FULL);
        package.source = Some("git+https://example.com/acme/frust-acme-ble#0123abcd".into());
        let entry = plugin_entry(&package, &package.metadata["frust"]["plugin"]).unwrap();
        assert!(matches!(
            entry.origin,
            PluginOrigin::Manifest {
                source: DepSource::Other,
                ..
            }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_value_that_is_not_a_table_is_refused() {
        let dir = temp_dir("not-table");
        let err = parse(&dir, "[package.metadata.frust]\nplugin = \"acme-ble\"\n").unwrap_err();
        assert_eq!(err.key, ROOT_KEY);
        let _ = fs::remove_dir_all(&dir);
    }

    /// One `packages[]` entry of canned `cargo metadata` output.
    fn metadata_package(name: &str, dir: &Path, source: Option<&str>, metadata: Value) -> Value {
        serde_json::json!({
            "name": name,
            "version": "0.2.0",
            "id": format!("{name}@0.2.0"),
            "source": source,
            "manifest_path": dir.join("Cargo.toml"),
            "features": { "scan": [] },
            "metadata": metadata,
            "dependencies": [],
        })
    }

    fn plugin_metadata(id: &str, extra: &str) -> Value {
        metadata_of(&format!(
            "[package.metadata.frust.plugin]\nid = \"{id}\"\nsummary = \"Plugin {id}\"\n{extra}"
        ))
    }

    #[test]
    fn discovery_reads_every_plugin_package_and_reports_the_invalid_ones() {
        let root = temp_dir("discovery");
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("Cargo.toml"), "[package]\nname = \"acme-app\"\n").unwrap();
        let path_plugin = root.join("plugins/acme-ble");
        fs::create_dir_all(path_plugin.join("platform/android")).unwrap();
        let registry_plugin = root.join("registry/frust-acme-scan-0.2.0");
        fs::create_dir_all(&registry_plugin).unwrap();
        let registry = "registry+https://github.com/rust-lang/crates.io-index";
        let android = "[package.metadata.frust.plugin.android]\n\
                       gradle-module = { name = \"frust-acme-ble\", path = \"platform/android\" }\n";

        let packages = vec![
            // The app's own table is never read as a plugin.
            metadata_package("acme-app", &app, None, plugin_metadata("acme-app", "")),
            metadata_package(
                "serde",
                &root.join("registry/serde"),
                Some(registry),
                Value::Null,
            ),
            metadata_package(
                "frust-acme-twin",
                &root.join("plugins/twin"),
                None,
                plugin_metadata("acme-ble", ""),
            ),
            metadata_package(
                "frust-acme-scan",
                &registry_plugin,
                Some(registry),
                plugin_metadata("acme-scan", ""),
            ),
            metadata_package(
                "frust-acme-broken",
                &root.join("plugins/broken"),
                None,
                plugin_metadata("Acme_Broken", ""),
            ),
            metadata_package(
                "docs-only",
                &root.join("registry/docs-only"),
                Some(registry),
                serde_json::json!({ "docs": { "rs": { "all-features": true } } }),
            ),
            metadata_package(
                "frust-acme-ble",
                &path_plugin,
                None,
                plugin_metadata("acme-ble", android),
            ),
        ];
        let metadata = serde_json::json!({
            "packages": packages,
            "workspace_members": ["acme-app@0.2.0"],
            "resolve": null,
            "version": 1,
        })
        .to_string();
        let runner = FakeProcessRunner::new().with(
            format!(
                "cargo metadata --format-version 1 --manifest-path {}",
                app.join("Cargo.toml").display()
            ),
            Output {
                success: true,
                stdout: metadata,
                stderr: String::new(),
            },
        );

        let found = discover(&app, &CargoLocator::new(&runner)).unwrap();

        let ids: Vec<&str> = found.plugins.iter().map(|e| &*e.spec.id).collect();
        assert_eq!(ids, ["acme-ble", "acme-scan"]);
        assert_eq!(
            found.plugins[0].origin,
            PluginOrigin::Manifest {
                package: "frust-acme-ble".to_string(),
                package_dir: path_plugin.clone(),
                version: "0.2.0".to_string(),
                source: DepSource::Path,
            }
        );
        assert!(
            found.plugins[0]
                .spec
                .base
                .contains(&Contribution::GradleModule {
                    gradle_name: s(":frust-acme-ble"),
                    rel_path: s("platform/android"),
                })
        );
        assert_eq!(
            found.plugins[1].origin,
            PluginOrigin::Manifest {
                package: "frust-acme-scan".to_string(),
                package_dir: registry_plugin.clone(),
                version: "0.2.0".to_string(),
                source: DepSource::Registry,
            }
        );
        let problems: Vec<(&str, &str)> = found
            .problems
            .iter()
            .map(|p| (p.package.as_str(), p.key.as_str()))
            .collect();
        let id_key = format!("{ROOT_KEY}.id");
        assert_eq!(
            problems,
            [
                ("frust-acme-broken", id_key.as_str()),
                ("frust-acme-twin", id_key.as_str()),
            ]
        );
        assert!(
            found.problems[1].reason.contains("frust-acme-ble"),
            "{}",
            found.problems[1]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_metadata_run_is_the_locators_error() {
        let app = temp_dir("discovery-fails");
        fs::write(app.join("Cargo.toml"), "[package]\nname = \"acme-app\"\n").unwrap();
        let runner = FakeProcessRunner::new().with(
            format!(
                "cargo metadata --format-version 1 --manifest-path {}",
                app.join("Cargo.toml").display()
            ),
            Output {
                success: false,
                stdout: String::new(),
                stderr: "error: no matching package".to_string(),
            },
        );
        assert!(matches!(
            discover(&app, &CargoLocator::new(&runner)),
            Err(PackagesError::MetadataFailed { .. })
        ));
        let _ = fs::remove_dir_all(&app);
    }
}
