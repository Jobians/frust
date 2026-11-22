# frust-onnx

**ONNX model inference** for frust apps, over [ONNX Runtime](https://onnxruntime.ai) through the
[`ort`](https://crates.io/crates/ort) crate. Load a model from a file or from bytes, read its
named inputs and outputs, and run it on typed tensors (f32, u8, i64). Execution is CPU only.

**Status:** desktop only. Linux (glibc, x86_64/aarch64), Apple-silicon macOS and Windows (MSVC)
link ONNX Runtime statically from a prebuilt archive fetched at build time. Android and iOS
packaging is not settled yet: those targets compile, but load the ONNX Runtime library at run
time and ship none. Intel macOS has no prebuilt archive and takes the same run-time route.

Like every frust **platform plugin**, this crate is added to your app's own `Cargo.toml`
alongside `frust`; the `frust` facade does not re-export it. It depends on no frust crate.

Building it needs Rust 1.92 or newer (the pinned `ort` requires it), above the workspace's 1.89.

---

## 1. Desktop quick start

```toml
# app Cargo.toml — [dependencies]
frust-onnx = { path = "<frust>/plugins/onnx" }  # or the crates.io release
```

```rust,ignore
use frust_onnx::{Session, Tensor};

let session = Session::from_file("model.onnx")?;
for input in session.inputs() {
    println!("{}: {} {:?}", input.name, input.dtype, input.shape); // None = dynamic dimension
}

let a = Tensor::from_f32([1, 3], vec![1.0, 2.0, 3.0])?;
let b = Tensor::from_f32([1, 3], vec![10.0, 20.0, 30.0])?;
for (name, tensor) in session.run_blocking(&[("a", a), ("b", b)])? {
    println!("{name}: {:?} {:?}", tensor.shape(), tensor.as_f32());
}
```

- `run_blocking` parks the calling thread until the run finishes: call it from a worker thread,
  never from the UI thread. `Session` is `Send + Sync`; runs on one session are serialised, so
  load one session per thread for parallel inference.
- Input names, element types, ranks and static dimensions are checked before the runtime is
  called (`OnnxError::InvalidInput`).
- One ONNX Runtime environment serves the process, created on first use. If it cannot be created,
  every call returns the same `OnnxError::Runtime`; nothing panics.
- `runtime_info()` reports the ONNX Runtime release and whether it is linked statically or loaded
  at run time.

### The build-time download

The first build on a machine downloads the prebuilt ONNX Runtime 1.31.0 archive for the target
(about 11 MB compressed for x86_64 Linux) from `cdn.pyke.io` and caches it, extracted, under
the user cache directory (`~/.cache/ort.pyke.io/dfbin/<target>/<hash>` on Linux,
`~/Library/Caches/ort.pyke.io/...` on macOS, `%LOCALAPPDATA%\ort.pyke.io\...` on Windows). Later
builds reuse the cache without network access. To build without the download, point
`ORT_LIB_PATH` at a local ONNX Runtime build; note that `ORT_SKIP_DOWNLOAD`, `ORT_OFFLINE` and
`cargo --offline` skip the download *and* the cache, which leaves the runtime unlinked.

On the run-time-loading targets the library is the one named by `ORT_DYLIB_PATH`, else the
platform's `onnxruntime` library on the loader path; it must be ONNX Runtime 1.17 or newer.

---

## 2. Pin policy

`ort` is pinned exactly (`=2.0.0-rc.15`, binding ONNX Runtime 1.31.0) in the workspace root
`Cargo.toml`, with default features off. The pin is frust-owned: it moves only together with this
crate's gate suite (`cargo test -p frust-onnx` on each desktop prebuilt row), never on its own.
Each desktop target's `ort` feature set must equal one prebuilt row of `ort`'s distribution table
exactly; `lax-feature-matching` is never turned on.

## 3. License

The crate is MIT OR Apache-2.0. A binary that ships it ships ONNX Runtime (MIT, Microsoft) and
the third-party components ONNX Runtime contains: see [`NOTICE`](NOTICE).
