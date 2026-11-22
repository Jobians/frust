# frust-onnx

**ONNX model inference** for frust apps, over [ONNX Runtime](https://onnxruntime.ai) through the
[`ort`](https://crates.io/crates/ort) crate. Load a model from a file or from bytes, read its
named inputs and outputs, and run it on typed tensors (f32, u8, i64). Execution is CPU only.

**Status:** desktop and arm64 Android. Linux (glibc, x86_64/aarch64), Apple-silicon macOS,
Windows (MSVC) and Android arm64-v8a link ONNX Runtime statically from a prebuilt archive fetched
at build time; Android also needs the NDK's `libc++_shared.so` in the APK (see
[Android packaging](#3-android-packaging)). iOS packaging is not settled yet: it compiles, but
loads the ONNX Runtime library at run time and ships none. Intel macOS, 32-bit Android and x86_64
Android take the same run-time route.

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

## 3. Android packaging

**Route: the prebuilt static archive.** On `aarch64-linux-android` `ort` downloads its prebuilt
ONNX Runtime 1.31.0 archive (feature set `nnapi`, the only Android row; 11.2 MB compressed,
a 132 MB `libonnxruntime.a` once extracted) and links it statically into the app's own `.so`.
The APK carries no `libonnxruntime.so`, so no Gradle module and no Maven dependency are needed.
The alternative, `load-dynamic` over the `onnxruntime-android` AAR from Maven Central, was not
needed and was not tried.

**What the app must add: `libc++_shared.so`.** The archive is built against the NDK's shared
C++ runtime, so the app's `.so` gains `NEEDED libc++_shared.so`. `cargo ndk` stages that library
next to the app's `.so` only when asked (`--link-libcxx-shared`); without it the APK still
builds, but ships an app `.so` whose dependency is missing, which the loader refuses at start-up.
In the scaffold's `android/app/build.gradle.kts`, the `cargoNdkBuild` task's command line gains
one argument:

```kotlin
add("cargo")
add("ndk")
add("--link-libcxx-shared") // frust-onnx: ONNX Runtime needs libc++_shared.so in the APK
add("--platform")
```

Setting `CARGO_NDK_LINK_LIBCXX_SHARED=true` in the environment of `frust build` does the same
for one build (`1` is rejected; the value must be `true`). AGP strips the staged copy (9.2 MB in
the NDK, 1.25 MB in the APK).

**Floors and ABIs.** minSdk 29: the scaffold's floor, and the archive links at API level 29.
The NDK is the scaffold's r28 (28.2.13676358), which aligns every `.so` to 16 KB pages.

| ABI | What happens |
|-----|--------------|
| arm64-v8a | Static link, as above. Builds, links and packages; the device run is still owed. |
| x86_64 | `ort` has an `nnapi` prebuilt row for it too, but this crate does not use it yet (no build has proven it), so it takes the run-time route: it compiles, and every session returns `OnnxError::Runtime` unless the app ships its own `libonnxruntime.so`. |
| armeabi-v7a | No prebuilt row: the run-time route, as for x86_64. |

**Execution providers in the shipped library.** CPU and NNAPI are compiled in (the archive
defines `onnxruntime::NnapiExecutionProvider`); XNNPACK is not (the archive defines no XNNPACK
symbol). This crate registers no provider, so runs are CPU only.

**Measured** on a fresh `frust create` app (Material counter) built with `frust build apk
--release --target-platform android-arm64`, with and without `frust-onnx` plus a ten-run
U2-NetP smoke (`u2netp.onnx`, 4,574,861 bytes, SHA-256
`309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8`, pushed with `adb push` to
the app's `/sdcard/Android/data/<application id>/files/`, not bundled):

| Measure | Value |
|---------|-------|
| Release APK without `frust-onnx` | 10,793,449 bytes (app `.so` 10,551,048) |
| Release APK with `frust-onnx` | 31,195,333 bytes (+20.4 MB): app `.so` 29,691,288 (+19.1 MB), `libc++_shared.so` 1,253,544 |
| 16 KB page size (`scripts/ci/page-size-check.sh` on the APK's `lib/arm64-v8a`) | both `.so` files pass, minimum PT_LOAD alignment 0x4000 |
| Clean `cargo ndk -t arm64-v8a build --release -p frust-onnx` | 9.2 s wall, the archive download included |
| ms per inference (median, worst), peak RSS | not measured yet: the device was locked for the whole run window |

The device row is owed: run the smoke on a physical arm64 phone and fill it in.

## 4. License

The crate is MIT OR Apache-2.0. A binary that ships it ships ONNX Runtime (MIT, Microsoft) and
the third-party components ONNX Runtime contains: see [`NOTICE`](NOTICE).
