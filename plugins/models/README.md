# frust-models

The one place on-device model files live in a Frust app. It lays files out under the app data
directory, keeps a sidecar manifest beside each, and lists, imports and removes them. It does no
inference and knows no model format: runtime plugins and apps ask it for a path and take it from
there.

```rust,no_run
use frust_models::{ModelId, ModelStore};

let store = ModelStore::open()?;
let id = ModelId::hub("acme/tiny", "onnx/model.onnx")?;
if let Some(model) = store.get(&id) {
    println!("{} bytes at {}", model.size_bytes, model.path.display());
}
# Ok::<(), frust_models::ModelsError>(())
```

## Where models live

The store root is `<data_dir>/frust-models`, where `data_dir` is `frust_paths::data_dir()`. A
model is identified by `namespace/name/path`; for Hugging Face files that is `author/repo/file`.
The file sits at `<root>/<namespace>/<name>/<path>`, and `path` may nest, for example
`onnx/model.onnx`.

Two sidecars sit next to each file:

- `<file>.part` is a download in progress. `ModelStore::in_progress` reports its size so a
  download can resume. Finishing renames it to the final name.
- `<file>.frust-meta.json` is the manifest: source (Hub repo and revision, URL, or local import), size,
  optional SHA-256, store time, licence and whether the content was verified. A manifest that
  cannot be parsed is logged and treated as absent.

Identifier components may not be empty, start with a dot, end in `.part` or `.frust-meta.json`, or contain
`\`, `:` or control characters, so no identifier can name a sidecar or leave the root.

On iOS and macOS the root and every finished file are excluded from device backups, since models
are large and can be fetched again. This is best effort: a failure is logged and the file still
works.

On Android `frust_paths::data_dir()` is unavailable until the shell has initialised, so
`ModelStore::open` returns `ModelsError::Storage` if it is called earlier. Open the store after
the app has started rather than in a static initialiser.
