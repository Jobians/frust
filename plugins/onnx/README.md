# frust-onnx

**ONNX model inference** for frust apps, over [ONNX Runtime](https://onnxruntime.ai) through the
[`ort`](https://crates.io/crates/ort) crate. Load a model from a file or from bytes, read its
named inputs and outputs, and run it on typed tensors (f32, u8, i64), blocking or on a
per-session worker thread with cancel. CoreML accelerates Apple targets; everything else runs on
the CPU, and an accelerator that cannot be used falls back to the CPU.

**Status:** desktop and arm64 Android. Linux (glibc, x86_64/aarch64), Apple-silicon macOS,
Windows (MSVC) and Android arm64-v8a link ONNX Runtime statically from a prebuilt archive fetched
at build time; Android also needs the NDK's `libc++_shared.so` in the APK (see
[Android packaging](#6-android-packaging)). iOS packaging is not settled yet: it compiles, but
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
  never from the UI thread, or use `Session::run` ([Threading](#3-threading)). `Session` is
  `Send + Sync`; runs on one session are serialised, so load one session per thread for parallel
  inference.
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

## 2. Sessions and accelerators

```rust,ignore
use frust_onnx::{Accelerator, Optimization, Provider, Session, SessionOptions};

let options = SessionOptions::default()
    .with_accelerator(Accelerator::Auto) // the default
    .with_intra_threads(2)
    .with_optimization(Optimization::All);
let session = Session::from_file_with("model.onnx", &options)?;
if session.provider() == Provider::Cpu { /* no accelerator on this device */ }
```

`Session::from_file` and `from_bytes` are `from_file_with`/`from_bytes_with` with
`SessionOptions::default()`. `SessionOptions` is `Clone + Eq + Hash`; set its fields or use the
`with_*` methods.

| Option | Default | Effect |
|--------|---------|--------|
| `accelerator` | `Auto` | `Auto`, `Cpu`, `CoreMl(CoreMlUnits::{All, CpuAndGpu, CpuAndNeuralEngine})` or `Xnnpack` |
| `strict_accelerator` | `false` | an explicit accelerator that cannot be used is `OnnxError::Unsupported` instead of a CPU fall back |
| `intra_threads` | `None` = `min(4, available_parallelism)` | threads one run uses, the calling one included (`Some(0)` counts as 1) |
| `optimization` | `All` | graph optimisation: `Basic`, `Extended` or `All` |
| `cpu_arena` | `false` | ONNX Runtime's CPU memory arena |
| `memory_pattern` | `false` | pre-plan one block for a run's intermediate tensors |
| `arena_shrinkage` | `false` | give unused arena blocks back after each run (only with `cpu_arena`) |

**Accelerator policy.** `Auto` is CoreML on all compute units on Apple targets and the CPU
everywhere else. On Android that is the CPU because the shipped ONNX Runtime archive has no
XNNPACK provider (see [Android packaging](#6-android-packaging)); NNAPI is compiled into that
archive but this crate never registers it. `Xnnpack` is accepted but compiled into no build yet,
so it always falls back (or is `Unsupported` when strict). When the provider is missing from the
build or from the runtime library, or the runtime refuses it while preparing the model, the load
logs a warning and retries on the CPU; `Session::provider()` reports what actually runs. With
`strict_accelerator`, a model the CPU loads but the accelerator does not is `Unsupported`, while
a model that fails on the CPU too stays `OnnxError::Load`. `Auto` never fails for want of a
provider, strict or not.

| Target | Provider library | `Auto` |
|--------|------------------|--------|
| Apple-silicon macOS | CoreML in the prebuilt `coreml` archive, linked statically | CoreML |
| iOS, Intel macOS | CoreML if the ONNX Runtime library loaded at run time has it | CoreML, else CPU |
| Android arm64 | static `nnapi` archive: CPU (and NNAPI, unused), no XNNPACK | CPU |
| Linux, Windows, others | CPU | CPU |

CoreML needs iOS 13 or newer; nodes it cannot run stay on the CPU. For a model loaded from a file,
CoreML keeps its compiled model under `<cache dir>/frust-onnx/coreml` (`frust_paths::cache_dir()`);
a model from bytes gets no cache. The CoreML code compiles only for Apple targets: it is
type-checked for `aarch64-apple-ios` but has not yet been linked or run on an Apple device.

**Memory.** U2-NetP (`u2netp.onnx`, 4.4 MiB) at 1x3x320x320, x86_64 Linux host (i5-12600,
12 threads), release build, ONNX Runtime 1.31.0, one fresh process per row, one warm-up run then
20 timed runs. Peak RSS is `VmHWM`, about 8 MiB before the first session; "after" is `VmRSS` once
the runs end. Rows vary by about 20 MiB between processes.

| Options (`All` unless noted, 4 threads unless noted) | Peak RSS | Resident after | Median per run |
|---------|---------:|---------:|---------:|
| arena on, pattern on (ONNX Runtime's defaults), 6 threads | 511 MiB | 511 MiB | 104 ms |
| arena on, pattern on | 510 MiB | 510 MiB | 114 ms |
| arena on, pattern on, shrinkage | 360 MiB | 110 MiB | 123 ms |
| arena on, pattern off | 317 MiB | 317 MiB | 113 ms |
| arena on, pattern off, shrinkage | 350 MiB | 97 MiB | 121 ms |
| arena off, pattern on | 314 MiB | 64 MiB | 121 ms |
| **arena off, pattern off (the default)** | **328 MiB** | 201 MiB | **120 ms** |
| the default, 1 thread | 301 MiB | 84 MiB | 385 ms |
| the default, 2 threads | 343 MiB | 85 MiB | 205 ms |
| the default, 6 threads | 340 MiB | 199 MiB | 102 ms |
| the default, `Extended` | 235 MiB | 84 MiB | 140 ms |
| the default, `Basic` | 238 MiB | 85 MiB | 144 ms |
| three sessions, arena on, pattern on | 1,460 MiB | 1,460 MiB | 115 ms |
| three sessions, arena on, pattern off, shrinkage | 690 MiB | 432 MiB | 120 ms |
| **three sessions, the default** | **332 MiB** | 85 MiB | 125 ms |

The arena with a memory pattern holds the first run's whole plan, rounded up, for the session's
life, and each session keeps its own: that is the 133 MB to 607 MB jump one U2-NetP session
caused on a OnePlus 9. With both off, the growth over the process falls from about 500 MiB to
about 320 MiB for one session (three sessions: about 1,450 MiB to 325 MiB) for about 5 % more
time per run, and the memory goes back to the allocator between runs. `Extended` cuts the peak
by another 80 MiB at about 15 % more time; it is not the default because the cost depends on the
model. Reproduce a row with the ignored `measure::measure_model` test (`tests/session.rs`), one
process per row:

```sh
FRUST_ONNX_MODEL=/path/to/u2netp.onnx FRUST_ONNX_ARENA=0 FRUST_ONNX_PATTERN=0 \
  cargo test --release -p frust-onnx --test session -- --ignored --exact measure::measure_model --nocapture
```

## 3. Threading

`Session::run` runs on the session's own worker thread, `frust-onnx-run`, and returns a
`RunHandle` at once:

```rust,ignore
use std::sync::Arc;
use frust_onnx::{RunEvent, Session, Tensor};

let session = Arc::new(Session::from_file("model.onnx")?);
let inputs = vec![("input".to_owned(), Tensor::from_f32([1, 3], vec![0.0; 3])?)];
let handle = session.run(inputs, Arc::new(|event| match event {
    RunEvent::Done(outputs) => { /* hand the outputs to the UI thread */ }
    RunEvent::Cancelled => {}
    RunEvent::Error(err) => log::warn!("{err}"),
}));
handle.cancel(); // or handle.wait(), handle.is_finished()
```

- The sink receives **exactly one** `RunEvent`, on the worker thread; a panicking sink is logged
  and the thread carries on. Post the result to the UI thread from the sink.
- One thread per session, started on the first `run`: runs on one session queue in submission
  order, and queued runs keep the session alive. The thread ends once the session is dropped and
  its queue is empty. It holds no thread-local state.
- `cancel()` on a queued run skips it; on a running one it sets ONNX Runtime's terminate flag
  (`RunOptions::terminate` in the pinned `ort`), which stops the graph at the next node boundary.
  Either way the sink receives `RunEvent::Cancelled`, even if the run had already finished but
  its outcome was not yet delivered. A cancel after delivery does nothing.
- `wait()` blocks until the sink has returned; never call it from the sink. Dropping the handle
  neither cancels nor waits.
- If no thread can be started, the sink receives `RunEvent::Error` on the calling thread before
  `run` returns.

## 4. Session cache

```rust,ignore
use frust_onnx::{SessionCache, SessionOptions};

let cache = SessionCache::default(); // at most 3 sessions
let session = cache.get_or_load("model.onnx", &SessionOptions::default())?;
// on the platform's memory-pressure signal:
cache.clear();
```

- Keyed by the canonical model path and the `SessionOptions`; the least recently used session is
  evicted when a new key would exceed `max_sessions`. Eviction and `clear()` drop only the
  cache's reference; a session still held elsewhere lives until its holder lets go.
- Threads asking for the same key at once share one load; a failed load is not cached.
  `evict(path)` drops every session for a path, whatever its options. `load_count()` counts the
  loads the cache has made.
- The default bound is 3 (`SessionCache::DEFAULT_MAX_SESSIONS`): three U2-NetP sessions with the
  default options peak at about 330 MiB on the host (table above), well under 1 GB. With
  ONNX Runtime's own memory defaults the same three reach 1,460 MiB, which is why those defaults
  are off. Larger models need a smaller bound, `SessionCache::new(n)`.

## 5. Pin policy

`ort` is pinned exactly (`=2.0.0-rc.15`, binding ONNX Runtime 1.31.0) in the workspace root
`Cargo.toml`, with default features off. The pin is frust-owned: it moves only together with this
crate's gate suite (`cargo test -p frust-onnx` on each desktop prebuilt row), never on its own.
Each desktop target's `ort` feature set must equal one prebuilt row of `ort`'s distribution table
exactly; `lax-feature-matching` is never turned on.

## 6. Android packaging

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
symbol). This crate registers neither NNAPI nor (absent) XNNPACK, so Android runs on the CPU and
`Accelerator::Auto` is `Cpu` there.

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
| U2-NetP on a OnePlus 9, ONNX Runtime's memory defaults (arena and memory pattern on) | 458 ms median per run; app peak RSS 133 MB before the session, 607 MB after (native heap 439 MB) |
| U2-NetP on a OnePlus 9, this crate's defaults | not measured yet: repeat the smoke with `SessionOptions::default()` |

The second device row is owed; the host table in [Sessions and accelerators](#2-sessions-and-accelerators)
predicts the growth falling by about a third.

## 7. License

The crate is MIT OR Apache-2.0. A binary that ships it ships ONNX Runtime (MIT, Microsoft) and
the third-party components ONNX Runtime contains: see [`NOTICE`](NOTICE).
