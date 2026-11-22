//! Validated model identifiers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ModelsError;

const MAX_LEN: usize = 512;

/// Identifies one model file as `namespace/name/path`.
///
/// For Hugging Face models `namespace/name` is the `author/repo` pair and `path` is the file
/// inside the repository. The identifier maps one to one onto a relative path under the store
/// root, so every component is validated to stay inside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelId {
    namespace: String,
    name: String,
    path: String,
}

fn invalid(id: &str, why: &str) -> ModelsError {
    ModelsError::InvalidModelId(format!("{id:?}: {why}"))
}

fn check_component(whole: &str, part: &str) -> Result<(), ModelsError> {
    if part.is_empty() {
        return Err(invalid(whole, "empty component"));
    }
    if part == "." || part == ".." || part.starts_with('.') {
        return Err(invalid(whole, "component must not start with a dot"));
    }
    if part.ends_with(".part") || part.ends_with(".json") {
        return Err(invalid(whole, "component uses a reserved suffix"));
    }
    if part
        .chars()
        .any(|c| c == '\\' || c == ':' || c == '\0' || c.is_control())
    {
        return Err(invalid(whole, "component contains a forbidden character"));
    }
    Ok(())
}

impl ModelId {
    /// Builds an identifier from its three parts. `path` may hold `/`-separated components.
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<Self, ModelsError> {
        let (namespace, name, path) = (namespace.into(), name.into(), path.into());
        let whole = format!("{namespace}/{name}/{path}");
        if whole.len() > MAX_LEN {
            return Err(invalid(&whole, "longer than 512 bytes"));
        }
        for part in [namespace.as_str(), name.as_str()] {
            if part.contains('/') {
                return Err(invalid(&whole, "namespace and name are single components"));
            }
            check_component(&whole, part)?;
        }
        for part in path.split('/') {
            check_component(&whole, part)?;
        }
        Ok(Self {
            namespace,
            name,
            path,
        })
    }

    /// Builds an identifier from a Hub `author/repo` id and a file path inside the repository.
    pub fn hub(repo_id: &str, path: impl Into<String>) -> Result<Self, ModelsError> {
        match repo_id.split_once('/') {
            Some((namespace, name)) => Self::new(namespace, name, path),
            None => Err(invalid(repo_id, "repository id must be `author/repo`")),
        }
    }

    /// The first component.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The second component.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The file path below `namespace/name`, `/`-separated.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `namespace/name`.
    pub fn repo_id(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    /// The last component of `path`.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The lowercased extension of the file name, when it has one.
    pub fn extension(&self) -> Option<String> {
        let file = self.file_name();
        file.rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .filter(|ext| !ext.is_empty())
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.namespace, self.name, self.path)
    }
}

impl FromStr for ModelId {
    type Err = ModelsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.splitn(3, '/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(ns), Some(name), Some(path)) => Self::new(ns, name, path),
            _ => Err(invalid(s, "expected `namespace/name/path`")),
        }
    }
}

impl Serialize for ModelId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ModelId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
