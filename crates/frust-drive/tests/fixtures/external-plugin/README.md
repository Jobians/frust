# External plugin fixture

`frust-ext-plugin-fixture` is a frust plugin that lives outside the frust plugin
registry. Its native requirements are declared only in its own `Cargo.toml`, under
`[package.metadata.frust.plugin]`:

- an Android Gradle module (`platform/android`, Kotlin class
  `dev.example.extplugin.ExtPluginHost`) and the `android.permission.NFC` permission;
- an iOS Swift package (`platform/ios`, `ExtPlugin.greeting()`), the `CoreNFC`
  framework, a string Info.plist key and an array Info.plist key.

`tests/external_plugin.rs` scaffolds an app that depends on this directory by path,
then drives discovery, `frust plugin sync` and the platform wiring against it. The
device gate below shows that the same wiring builds, installs and runs on real
toolchains.

## Device gate

Run it with a `frust` binary built from the checkout under test
(`cargo build -p frust-cli`, then use `target/debug/frust`), in a scratch directory
outside the repository:

```sh
frust create ext-gate --project-name ext_gate --frust-path <checkout> --platforms android,ios
cp -R <checkout>/crates/frust-drive/tests/fixtures/external-plugin ext-plugin
cd ext-gate
# Cargo.toml [dependencies]:
#   frust-ext-plugin-fixture = { path = "../ext-plugin" }
# src/lib.rs: show frust_ext_plugin_fixture::GREETING and the native string in text(...)
git init && git add -A && git commit -m baseline
```

The fixture's Rust side exports only the `GREETING` constant, so the scratch app
supplies its own bridge to the native string. On iOS, declare an `@objc` class in
`ios/Runner/AppDelegate.swift` that returns `ExtPlugin.greeting()`, and call it from
Rust through `objc_getClass` / `objc_msgSend`. A plain `extern "C"` symbol that is
defined only in the Runner target does not work: the app crate is also built as a
`cdylib`, and that link fails on the undefined symbol.

| # | Command | Expected |
|---|---------|----------|
| 1 | `frust plugin list` | `ext-fixture  frust-ext-plugin-fixture 0.1.0 (path)  7 pending` |
| 2 | `frust run -d <device>` before sync | stops with `plugin ext-fixture (frust-ext-plugin-fixture) is not wired into this project: 7 change(s) pending`; no Gradle or xcodebuild run; no tracked file modified |
| 3 | `frust plugin sync`, twice; `git diff --stat` | changes `settings.gradle.kts`, `app/build.gradle.kts`, `AndroidManifest.xml`, `project.pbxproj`, `Info.plist` and one `.gitignore` line `/ios/ExtPlugin`; the second sync reports up to date |
| 4 | `frust run -d <android phone>` | the Kotlin string is on screen |
| 5 | `frust run -d <iPhone>` (signed) | Xcode resolves `ExtPlugin` through the `ios/ExtPlugin` link; the Swift string is on screen |
| 6 | `frust build apk`; `frust build ios --no-codesign` | both succeed |
| 7 | move `ext-plugin`, fix the path, `frust run` again | `android/local.properties` and `ios/ExtPlugin` follow the new location; only `Cargo.toml` is modified |
| 8 | add `entitlements = { "com.apple.developer.nfc.readersession.formats" = ["TAG"] }` and a group-restricted `maven-repositories` entry to the copied manifest; sync | (a) Gradle still evaluates `settings.gradle.kts`; (b) an unsigned iOS build succeeds with `CODE_SIGN_ENTITLEMENTS` set; (c) whether a signed build accepts the entitlement |
| 9 | add a `Debug-Dev` Runner configuration, then sync the entitlement (iOS); sync an app without `ios/` (Android) | iOS: `CODE_SIGN_ENTITLEMENTS` lands in `Debug-Dev` too. Android: only the Android files change |

Checks 1 to 7 must pass on both platforms. Checks 8 and 9 are informational.

## Results

Android was walked on a Linux host on 2026-11-18 (Pixel 4a, Android 16; Gradle 9.5.1,
AGP 9.3.0, Kotlin 2.3.20, JDK 17, NDK 28.2.13676358). Checks 1 to 9 ran with frust at
`bc711b2d`; the release APK and the `.gitignore` line were re-checked at `0a22d568`.
iOS was walked on a macOS host on 2026-11-19 (iPhone 13 mini, iOS 27.2; Xcode 27.0,
macOS 27.0.1) with frust at `0a22d568`.

| # | Android | iOS |
|---|---------|-----|
| 1 | PASS: `6 pending` at `bc711b2d`, `7 pending` at `0a22d568` | PASS: `7 pending` |
| 2 | PASS: stopped, no Gradle run, no tracked change | PASS: stopped, no xcodebuild run, no tracked change |
| 3 | PASS: 6 files, 42 insertions at `0a22d568`; second sync up to date | PASS: same 6 files, 42 insertions; second sync up to date. Sync prints `6 applied` against list's 7 pending (known) |
| 4 | PASS: `hello from ext-fixture (android)` on screen | n/a |
| 5 | n/a | PASS: signed debug build installed; `hello from ext-fixture (ios)` on screen |
| 6 | PASS: release APK keeps `ExtPluginHost` (consumer keep rules) and shows the Kotlin string | PASS: `frust build ios --no-codesign` |
| 7 | PASS: only `Cargo.toml` modified | PASS: link re-pointed, app relaunched, only `Cargo.toml` modified; the link is ignored |
| 8 | (a) PASS: `./gradlew help --offline` succeeds with the repository block | (b) PASS: unsigned build with `CODE_SIGN_ENTITLEMENTS`, `Runner.entitlements` created. (c) PASS: with an account signed in to Xcode, automatic signing created an explicit App ID profile and the signed app carries the entitlement. Without a signed-in account the build is refused, because the team wildcard profile has no NFC Tag Reading capability |
| 9 | PASS: Android-only app, only the Android files changed | PASS: `CODE_SIGN_ENTITLEMENTS` written to Debug, Debug-Dev, Release and Profile |
