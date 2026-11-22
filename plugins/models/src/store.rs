//! The on-disk model store and its sidecar manifests.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::apple::exclude_from_backup;
use crate::error::ModelsError;
use crate::id::ModelId;

const PART_SUFFIX: &str = ".part";
const MANIFEST_SUFFIX: &str = ".frust-meta.json";

/// Where a stored file came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ModelSource {
    /// A file in a Hugging Face repository at a pinned revision.
    HuggingFace {
        /// `author/repo`.
        repo: String,
        /// Commit or ref the file was resolved at.
        revision: String,
    },
    /// A plain HTTPS download.
    Url {
        /// The source URL.
        url: String,
    },
    /// A file copied in from the local filesystem.
    Imported {
        /// The path it was copied from, when known.
        original_path: Option<String>,
    },
}

/// The sidecar record kept beside each stored file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifest {
    /// Where the file came from.
    pub source: ModelSource,
    /// Size of the stored file in bytes.
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the file, when known.
    pub sha256: Option<String>,
    /// Seconds since the Unix epoch when the file was stored.
    pub downloaded_at_unix: u64,
    /// Licence identifier, when known.
    pub licence: Option<String>,
    /// Whether the content was checked against a published digest.
    pub verified: bool,
}

/// A model file present in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModel {
    /// Identifier of the file.
    pub id: ModelId,
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Size on disk in bytes.
    pub size_bytes: u64,
    /// The sidecar manifest, when present and readable.
    pub manifest: Option<ModelManifest>,
}

/// Handle to the model directory.
#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
}

fn with_suffix(path: PathBuf, suffix: &str) -> PathBuf {
    let mut os = path.into_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl ModelStore {
    /// Opens the store under the app data directory, creating it when missing.
    ///
    /// On Android the data directory exists only once the shell has initialised.
    pub fn open() -> Result<Self, ModelsError> {
        let base = frust_paths::data_dir().ok_or_else(|| {
            ModelsError::Storage(
                "data directory unavailable (on Android, open the store after the shell initialised)"
                    .into(),
            )
        })?;
        Self::with_root(base.join("frust-models"))
    }

    /// Opens a store rooted at `root`, creating it when missing.
    pub fn with_root(root: PathBuf) -> Result<Self, ModelsError> {
        fs::create_dir_all(&root)?;
        exclude_from_backup(&root);
        Ok(Self { root })
    }

    /// The store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the finished file for `id` lives.
    pub fn path_for(&self, id: &ModelId) -> PathBuf {
        let mut path = self.root.join(id.namespace()).join(id.name());
        for part in id.path().split('/') {
            path.push(part);
        }
        path
    }

    /// Where a partial download of `id` lives.
    pub fn part_path_for(&self, id: &ModelId) -> PathBuf {
        with_suffix(self.path_for(id), PART_SUFFIX)
    }

    /// Where the sidecar manifest of `id` lives.
    pub fn manifest_path_for(&self, id: &ModelId) -> PathBuf {
        with_suffix(self.path_for(id), MANIFEST_SUFFIX)
    }

    fn read_manifest(&self, id: &ModelId) -> Option<ModelManifest> {
        let path = self.manifest_path_for(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                log::warn!("cannot read manifest {}: {err}", path.display());
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(manifest) => Some(manifest),
            Err(err) => {
                log::warn!("ignoring malformed manifest {}: {err}", path.display());
                None
            }
        }
    }

    fn local(&self, id: ModelId, size_bytes: u64) -> LocalModel {
        LocalModel {
            path: self.path_for(&id),
            manifest: self.read_manifest(&id),
            id,
            size_bytes,
        }
    }

    /// Walks the store and calls `visit` with each file's id, size and whether it is a `.part`.
    fn walk(&self, visit: &mut dyn FnMut(ModelId, u64, bool)) {
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&self.root) else {
                    continue;
                };
                let Some(rel) = rel.to_str() else { continue };
                let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");
                if rel.ends_with(MANIFEST_SUFFIX) {
                    continue;
                }
                let (rel, is_part) = match rel.strip_suffix(PART_SUFFIX) {
                    Some(stripped) => (stripped, true),
                    None => (rel.as_str(), false),
                };
                if let Ok(id) = rel.parse::<ModelId>() {
                    visit(id, meta.len(), is_part);
                }
            }
        }
    }

    /// Every finished model, sorted by id.
    pub fn list(&self) -> Vec<LocalModel> {
        let mut found = Vec::new();
        self.walk(&mut |id, size, is_part| {
            if !is_part {
                found.push((id, size));
            }
        });
        found.sort();
        found
            .into_iter()
            .map(|(id, size)| self.local(id, size))
            .collect()
    }

    /// Every finished model of one repository, sorted by id.
    pub fn list_in(&self, namespace: &str, name: &str) -> Vec<LocalModel> {
        self.list()
            .into_iter()
            .filter(|m| m.id.namespace() == namespace && m.id.name() == name)
            .collect()
    }

    /// The finished model `id`, when present.
    pub fn get(&self, id: &ModelId) -> Option<LocalModel> {
        let meta = fs::metadata(self.path_for(id)).ok()?;
        meta.is_file().then(|| self.local(id.clone(), meta.len()))
    }

    /// Deletes the file, its partial download and its manifest. Missing files are not an error.
    pub fn remove(&self, id: &ModelId) -> Result<(), ModelsError> {
        for path in [
            self.path_for(id),
            self.part_path_for(id),
            self.manifest_path_for(id),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        let mut dir = self.path_for(id);
        while dir.pop() && dir != self.root && dir.starts_with(&self.root) {
            if fs::remove_dir(&dir).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// Copies `source` into the store as `id` and records an `Imported` manifest.
    pub fn import(
        &self,
        source: &Path,
        id: &ModelId,
        licence: Option<String>,
    ) -> Result<LocalModel, ModelsError> {
        let part = self.part_path_for(id);
        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Err(err) = fs::copy(source, &part) {
            let _ = fs::remove_file(&part);
            return Err(err.into());
        }
        let size_bytes = fs::metadata(&part)?.len();
        let manifest = ModelManifest {
            source: ModelSource::Imported {
                original_path: source.to_str().map(str::to_owned),
            },
            size_bytes,
            sha256: None,
            downloaded_at_unix: now_unix(),
            licence,
            verified: false,
        };
        self.finalize_file(id, &manifest)
    }

    /// Partial downloads and their byte counts, sorted by id.
    pub fn in_progress(&self) -> Vec<(ModelId, u64)> {
        let mut found = Vec::new();
        self.walk(&mut |id, size, is_part| {
            if is_part {
                found.push((id, size));
            }
        });
        found.sort();
        found
    }

    /// Total bytes of finished models.
    pub fn total_bytes(&self) -> u64 {
        let mut total = 0;
        self.walk(&mut |_, size, is_part| {
            if !is_part {
                total += size;
            }
        });
        total
    }

    /// Promotes the partial file of `id` to its final name and writes its manifest.
    pub fn finalize_file(
        &self,
        id: &ModelId,
        manifest: &ModelManifest,
    ) -> Result<LocalModel, ModelsError> {
        let path = self.path_for(id);
        fs::rename(self.part_path_for(id), &path)?;
        exclude_from_backup(&path);
        let json = serde_json::to_vec_pretty(manifest)
            .map_err(|err| ModelsError::Storage(format!("cannot encode manifest: {err}")))?;
        frust_paths::atomic_write(&self.manifest_path_for(id), &json)?;
        let size = fs::metadata(&path)?.len();
        Ok(LocalModel {
            id: id.clone(),
            path,
            size_bytes: size,
            manifest: Some(manifest.clone()),
        })
    }
}
