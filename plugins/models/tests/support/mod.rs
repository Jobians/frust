//! Helpers shared by the download tests.
#![allow(dead_code)]

pub mod http_fixture;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use frust_models::ModelStore;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A scratch directory removed on drop.
pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("frust-models-dl-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn store(&self) -> ModelStore {
        ModelStore::with_root(self.0.join("root")).unwrap()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
