# onnx-vision

On-device vision in a Frust app, built the way an app author uses `frust-models` and
`frust-onnx`: download a verified model, then run it on a photo without blocking the UI.

Two destinations behind a Material 3 navigation bar:

- **Models**: one row per `frust_onnx::catalog::VISION_CATALOG` model (U2-NetP, YuNet, Depth
  Anything V2 Small) with its task, size, licence and input size, whether it is installed, and
  Download, a live progress line (bytes, percent, speed) from `DownloadTracker`, Cancel, Resume
  (starting again continues from the kept partial file) and Delete. Above the rows: the store's
  disk use (`ModelStore::total_bytes` plus partial downloads) and, on a device with less than
  4 GB of RAM (`DeviceMemory::query`), a note that one model session can peak near 380 MB.
- **Vision**: the bundled sample or a photo picked with the system picker, and three actions
  driven by `VisionHandle`: Subject mask (a tinted alpha layer over the photo), Faces (boxes
  and the five landmarks drawn over it) and Depth (a grayscale map, near is white). An action
  stays disabled, with a "download the model first" hint, until its model is in the store. The
  status line names the execution provider that ran the session and the last run's time in
  milliseconds; a task's first run includes loading its model.

Nothing blocking runs on the UI thread: downloads run on their own threads, and photo
decoding, the 2048 px downscale, disk walks, overlay conversion and inference run on blocking
threads. Photos are downscaled to at most 2048 px on the long side before inference.

## Run it

```sh
cd examples/onnx-vision
cargo run                                   # desktop (Linux, macOS, Windows)
frust run -d <device>                       # an Android phone or the iOS simulator
frust build apk --target-platform android-arm64
```

- **Desktop**: ONNX Runtime is linked statically on Linux (glibc), Apple-silicon macOS and
  Windows. The first build downloads its prebuilt archive (about 11 MB) and caches it. Picked
  photos are decoded natively on macOS (HEIC included) and as PNG or JPEG on Linux and Windows.
- **Android**: arm64-v8a only. ONNX Runtime is linked statically into `libonnxvision.so`, which
  then needs the NDK's `libc++_shared.so` in the APK; `frust plugin add onnx` set
  `frust.cargoNdk.linkLibcxxShared=true` in `android/gradle.properties` for that. A project
  whose `android/app/build.gradle.kts` predates that property needs this line in the
  `cargoNdkBuild` command line, right after `add("ndk")`:

  ```kotlin
  if ((project.findProperty("frust.cargoNdk.linkLibcxxShared") as String?)?.trim().equals("true", ignoreCase = true)) add("--link-libcxx-shared")
  ```

  A release build needs signing values (`android/key.properties` or the `ANDROID_*`
  variables; see `frust.toml`).
- **iOS**: builds, but does not run the models yet. `frust-onnx` loads ONNX Runtime at run
  time on iOS and the app ships none (`frust plugin add onnx` makes no iOS edit until the iOS
  link gate settles it), so every task fails at session creation with `OnnxError::Runtime`,
  shown as a Failed status line. Downloads, the picker and the rest of the UI work.

The models are stored under the app data directory (`<data_dir>/frust-models`); Delete removes
a model, its partial download and its manifest.

## Tests

A standalone workspace (its own `[workspace]` and `Cargo.lock`, excluded from the root one), so
it gates from its own directory:

```sh
cd examples/onnx-vision
cargo test --locked && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
```

- `tests/app.rs` boots the app headless (`frust::testing::Tester`, no GPU and no window) and
  checks the Models screen lists three rows, and that with an empty store the three Vision
  actions are disabled with their hints once the sample has decoded.
- `tests/format.rs` pins the progress line for every `DownloadState`, the partial and storage
  lines and the low-memory threshold.
- `tests/live.rs` is ignored by default because it downloads: through the real components it
  downloads U2-NetP from the Models screen, runs Subject mask on the sample from the Vision
  screen, prints both times and checks the mask (the ball is subject, the sky is not):

  ```sh
  cargo test --test live -- --ignored --nocapture
  ```

## The sample image

`assets/sample.png` (640x480, about 42 KB) is generated, not photographed:
`assets/make_sample.py` draws a shaded ball in front of a block on a tiled floor under a sky
with nothing but arithmetic (standard-library Python), and writes the same bytes on every run:

```sh
python3 -I assets/make_sample.py assets/sample.png
```

It contains no person and no third-party material, and is covered by this repository's licence
(MIT OR Apache-2.0). With no face in it, Faces finds none on the sample: pick a photo to try it.

## How it was made

Scaffolded and wired with the `frust` CLI built from this checkout, run inside the new
directory so the relative `--frust-path` means the same thing to the CLI and to Cargo:

```sh
cargo build -p frust-cli                    # from the repository root
mkdir examples/onnx-vision && cd examples/onnx-vision
frust create . --platforms android,ios --frust-path ../.. --org it.f0x --project-name onnxvision \
  --description "On-device vision with frust-models and frust-onnx: download catalog models, then mask, faces and depth"
frust plugin add models
frust plugin add onnx
frust plugin add photo-library --feature decode
frust plugin add device-info
```

The plugin applier wrote the Cargo dependencies, the `:frust-photo-library` Gradle module, the
`PhotosUI` and `ImageIO` Xcode links and the `linkLibcxxShared` property. By hand: the
`[workspace]` header in `Cargo.toml` (as every standalone example has), the plugin paths
shortened from `../../crates/frust/../../plugins/<id>` to `../../plugins/<id>`, the
`frust-ui` `test-support` dev-dependency, this README, the sources and tests, the root
`Cargo.toml` `exclude` entry and the `crates/frust-cli/tests/profile_sync.rs` roster entry
(this manifest keeps the profile blocks `frust create` emitted).

| Piece | Where |
|-------|-------|
| Shell (app bar, navigation bar, both screens kept mounted), `frust::app!` | `src/lib.rs` |
| Shared handles: store, list, trackers, vision handle, run timing | `src/services.rs` |
| Models screen | `src/models_screen.rs` |
| Vision screen, overlay canvas | `src/vision_screen.rs` |
| Decoding, the 2048 px cap, overlay images | `src/imaging.rs` |
| Status text | `src/format.rs` |
| Generated Android and iOS projects | `android/`, `ios/` |

## Device status

| Device | OS | Download / cancel / resume / delete | Subject mask | Faces | Depth | Provider and timings | Notes |
|--------|----|-------------------------------------|--------------|-------|-------|----------------------|-------|
| | | | | | | | |
