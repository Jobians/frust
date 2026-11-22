use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use frust_models::{ModelId, ModelManifest, ModelSource, ModelStore, ModelsError};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("frust-models-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn store(&self) -> ModelStore {
        ModelStore::with_root(self.0.join("root")).unwrap()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(ns: &str, name: &str, path: &str) -> ModelId {
    ModelId::new(ns, name, path).unwrap()
}

fn manifest(source: ModelSource, size: u64) -> ModelManifest {
    ModelManifest {
        source,
        size_bytes: size,
        sha256: Some("ab".repeat(32)),
        downloaded_at_unix: 1_700_000_000,
        licence: Some("apache-2.0".into()),
        verified: true,
    }
}

fn stage(store: &ModelStore, id: &ModelId, bytes: &[u8], manifest: &ModelManifest) {
    let part = store.part_path_for(id);
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, bytes).unwrap();
    store.finalize_file(id, manifest).unwrap();
}

#[test]
fn layout_including_nested_path() {
    let tmp = Temp::new();
    let store = tmp.store();
    let nested = id("acme", "tiny", "onnx/model.onnx");
    let root = store.root();
    assert_eq!(
        store.path_for(&nested),
        root.join("acme")
            .join("tiny")
            .join("onnx")
            .join("model.onnx")
    );
    assert_eq!(
        store.part_path_for(&nested),
        root.join("acme/tiny/onnx/model.onnx.part")
    );
    assert_eq!(
        store.manifest_path_for(&nested),
        root.join("acme/tiny/onnx/model.onnx.json")
    );
    assert_eq!(nested.to_string(), "acme/tiny/onnx/model.onnx");
    assert_eq!(nested.repo_id(), "acme/tiny");
    assert_eq!(nested.file_name(), "model.onnx");
    assert_eq!(id("a", "b", "M.GGUF").extension().as_deref(), Some("gguf"));
    assert_eq!(
        ModelId::hub("acme/tiny", "x.bin").unwrap(),
        id("acme", "tiny", "x.bin")
    );
}

#[test]
fn manifest_round_trip_for_every_source() {
    let tmp = Temp::new();
    let store = tmp.store();
    let sources = [
        ModelSource::HuggingFace {
            repo: "acme/tiny".into(),
            revision: "abc123".into(),
        },
        ModelSource::Url {
            url: "https://example.com/m.bin".into(),
        },
        ModelSource::Imported {
            original_path: Some("/tmp/m.bin".into()),
        },
        ModelSource::Imported {
            original_path: None,
        },
    ];
    for (i, source) in sources.into_iter().enumerate() {
        let model = id("acme", "tiny", &format!("f{i}.bin"));
        let m = manifest(source, 3);
        stage(&store, &model, b"abc", &m);
        let got = store.get(&model).unwrap();
        assert_eq!(got.manifest.as_ref(), Some(&m));
        assert_eq!(got.size_bytes, 3);
    }
}

#[test]
fn malformed_manifest_is_no_manifest() {
    let tmp = Temp::new();
    let store = tmp.store();
    let model = id("acme", "tiny", "m.bin");
    stage(
        &store,
        &model,
        b"abc",
        &manifest(ModelSource::Url { url: "u".into() }, 3),
    );
    fs::write(store.manifest_path_for(&model), b"{ not json").unwrap();
    assert!(store.get(&model).unwrap().manifest.is_none());
}

#[test]
fn list_skips_sidecars_and_sorts() {
    let tmp = Temp::new();
    let store = tmp.store();
    let m = manifest(ModelSource::Url { url: "u".into() }, 1);
    for model in [
        id("zed", "m", "b.bin"),
        id("acme", "tiny", "onnx/model.onnx"),
        id("acme", "tiny", "a.bin"),
    ] {
        stage(&store, &model, b"x", &m);
    }
    fs::write(store.part_path_for(&id("acme", "tiny", "c.bin")), b"zz").unwrap();
    let ids: Vec<String> = store.list().iter().map(|l| l.id.to_string()).collect();
    assert_eq!(
        ids,
        [
            "acme/tiny/a.bin",
            "acme/tiny/onnx/model.onnx",
            "zed/m/b.bin"
        ]
    );
    assert_eq!(store.list_in("acme", "tiny").len(), 2);
    assert_eq!(store.total_bytes(), 3);
}

#[test]
fn remove_is_idempotent_and_prunes() {
    let tmp = Temp::new();
    let store = tmp.store();
    let model = id("acme", "tiny", "onnx/model.onnx");
    stage(
        &store,
        &model,
        b"x",
        &manifest(ModelSource::Url { url: "u".into() }, 1),
    );
    store.remove(&model).unwrap();
    store.remove(&model).unwrap();
    assert!(store.get(&model).is_none());
    assert!(!store.root().join("acme").exists());
    assert!(store.root().exists());

    let keep = id("acme", "tiny", "keep.bin");
    stage(
        &store,
        &keep,
        b"x",
        &manifest(ModelSource::Url { url: "u".into() }, 1),
    );
    store.remove(&model).unwrap();
    assert!(store.get(&keep).is_some());
}

#[test]
fn in_progress_reports_part_bytes() {
    let tmp = Temp::new();
    let store = tmp.store();
    let model = id("acme", "tiny", "big.bin");
    let part = store.part_path_for(&model);
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, vec![0u8; 1234]).unwrap();
    assert_eq!(store.in_progress(), vec![(model.clone(), 1234)]);
    assert!(store.list().is_empty());
    store.remove(&model).unwrap();
    assert!(store.in_progress().is_empty());
}

#[test]
fn import_round_trip() {
    let tmp = Temp::new();
    let store = tmp.store();
    let source = tmp.0.join("src.bin");
    fs::write(&source, b"hello").unwrap();
    let model = id("local", "mine", "src.bin");
    let imported = store.import(&source, &model, Some("mit".into())).unwrap();
    assert_eq!(imported.size_bytes, 5);
    let manifest = imported.manifest.unwrap();
    assert!(!manifest.verified);
    assert_eq!(manifest.licence.as_deref(), Some("mit"));
    assert!(matches!(
        manifest.source,
        ModelSource::Imported {
            original_path: Some(_)
        }
    ));
    assert_eq!(fs::read(store.path_for(&model)).unwrap(), b"hello");
    assert!(!store.part_path_for(&model).exists());
    assert_eq!(store.get(&model).unwrap().manifest.unwrap().size_bytes, 5);
}

#[test]
fn model_id_rejects_bad_input() {
    let bad = [
        ("a", "b", "../x"),
        ("a", "b", "x/../y"),
        ("a", "b", "x//y"),
        ("a", "b", ""),
        ("a", "b", "/x"),
        ("a", "b", "x/"),
        ("a", "b", ".hidden"),
        ("a", "b", "dir/.hidden"),
        ("a", "b", "m.bin.part"),
        ("a", "b", "m.bin.json"),
        ("a", "b", "x\\y"),
        ("a", "b", "c:d"),
        ("a", "b", "x\0y"),
        ("a", "b", "x\ny"),
        ("a/b", "c", "x"),
        ("a", "b/c", "x"),
        ("", "b", "x"),
        ("..", "b", "x"),
        (".", "b", "x"),
    ];
    for (ns, name, path) in bad {
        assert!(
            matches!(
                ModelId::new(ns, name, path),
                Err(ModelsError::InvalidModelId(_))
            ),
            "{ns}/{name}/{path} should be rejected"
        );
    }
    let long = "x".repeat(600);
    assert!(ModelId::new("a", "b", long).is_err());
    assert!(ModelId::hub("norepo", "x").is_err());
}

#[test]
fn model_id_serde_is_a_string() {
    let model = id("acme", "tiny", "onnx/model.onnx");
    let json = serde_json::to_string(&model).unwrap();
    assert_eq!(json, "\"acme/tiny/onnx/model.onnx\"");
    assert_eq!(serde_json::from_str::<ModelId>(&json).unwrap(), model);
    assert!(serde_json::from_str::<ModelId>("\"a/b/../c\"").is_err());
}
