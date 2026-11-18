//! [`PluginCatalog`]: the one list of plugins every consumer looks a plugin
//! up in — `frust plugin add`, the TUI's Add Plugin dialog, the machine-local
//! wiring ([`crate::platform_wiring`]) and the desktop-lane scan
//! ([`super::desktop_contributions`]).
//!
//! The built-in registry ([`known_plugins`]) is one source of entries; a
//! plugin described at run time is another, carried beside it with its own
//! [`PluginOrigin`]. Lookups never care which source an entry came from;
//! only the code paths that need a package layout do, and they say so.

use std::path::PathBuf;

use super::PluginSpec;
use super::registry::known_plugins;

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
    /// A registry (crates.io or alternate registry) dependency.
    Registry,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{Contribution, FeatureSpec};
    use std::borrow::Cow;

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
}
