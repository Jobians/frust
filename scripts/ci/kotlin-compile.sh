#!/usr/bin/env bash
# Kotlin compile check for the Android half of every built-in plugin.
#
# Nothing else in CI compiles Kotlin: cargo never does, and the Kotlin conformance
# tests only read source text. A Kotlin break in a plugin would otherwise reach users as a
# build failure in their own app. This script scaffolds a throwaway Android app, adds every
# built-in plugin that ships a platform/android Gradle module, and compiles the Kotlin of
# each of those modules plus the embedding. The scaffold keeps its default platforms because
# `frust plugin add` edits the iOS project too and refuses an app without one.
#
# Usage: scripts/ci/kotlin-compile.sh [gradle-args...]
#   Run from the repository root. Extra arguments go to every Gradle call, e.g. --offline for
#   a local run against a warm ~/.gradle cache.
#
# Environment:
#   FRUST_BIN  path of a prebuilt `frust` binary; when unset, `cargo build -p frust-cli` runs.
#
# Tasks run: `:<module>:compileDebugKotlin` for the embedding and for every plugin module
# the scaffold's settings.gradle.kts includes after the adds. That task is the narrowest one
# that type-checks a library's Kotlin. In a scaffolded app the Rust library is built by
# cargoNdkBuild, which hangs off the `:app` module's preBuild only, and no library module's
# compileDebugKotlin depends on `:app`, so naming the library tasks explicitly never builds
# Rust. The `:app` tasks are deliberately not run.
#
# The last line printed is `gradle exit status: <N>` and the script exits with N. A failure
# before Gradle runs (scaffold, plugin add) exits non-zero with its own message instead.
set -euo pipefail

root="$(pwd)"
[ -f "$root/Cargo.toml" ] && [ -d "$root/plugins" ] && [ -d "$root/crates/frust-cli" ] || {
  echo "run this script from the frust repository root" >&2
  exit 2
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ -n "${FRUST_BIN:-}" ]; then
  frust="$FRUST_BIN"
else
  cargo build -p frust-cli --bin frust
  target_dir="${CARGO_TARGET_DIR:-$root/target}"
  case "$target_dir" in /*) ;; *) target_dir="$root/$target_dir" ;; esac
  frust="$target_dir/debug/frust"
fi
[ -x "$frust" ] || { echo "no frust binary at $frust" >&2; exit 2; }

plugins=()
for dir in plugins/*/platform/android; do
  [ -d "$dir" ] || continue
  plugins+=("$(basename "$(dirname "$(dirname "$dir")")")")
done
[ "${#plugins[@]}" -gt 0 ] || { echo "no plugin with a platform/android module found" >&2; exit 2; }

app="$tmp/kotlincheck"
"$frust" create "$app" --org dev.frust.kotlincheck --project-name kotlincheck \
  --frust-path "$root" >"$tmp/create.log" 2>&1 || {
  cat "$tmp/create.log" >&2
  echo "frust create failed" >&2
  exit 1
}

for id in "${plugins[@]}"; do
  # secure-storage includes its Android module only through its biometric-gate feature.
  feature=()
  [ "$id" = secure-storage ] && feature=(--feature biometric-gate)
  if ! (cd "$app" && "$frust" plugin add "$id" ${feature[@]+"${feature[@]}"}) >"$tmp/add-$id.log" 2>&1; then
    cat "$tmp/add-$id.log" >&2
    echo "frust plugin add $id failed" >&2
    exit 1
  fi
done

# Every library module the scaffold includes (the embedding and each plugin), never :app.
modules=()
while IFS= read -r m; do
  modules+=("$m")
done < <(sed -n 's/^[[:space:]]*include(":\([^"]*\)").*/\1/p' "$app/android/settings.gradle.kts" | grep -vx app | sort -u)
# Every listed plugin must have landed as a module, not merely been accepted by `plugin add`.
for id in "${plugins[@]}"; do
  printf '%s\n' "${modules[@]}" | grep -qx "frust-$id" || {
    echo "plugin $id added but module frust-$id is not in settings.gradle.kts (found: ${modules[*]})" >&2
    exit 1
  }
done

tasks=()
for m in "${modules[@]}"; do
  tasks+=(":$m:compileDebugKotlin")
done

echo "plugins: ${plugins[*]}"
echo "tasks: ${tasks[*]}"

status=0
(cd "$app/android" && ./gradlew "${tasks[@]}" "$@") || status=$?
echo "gradle exit status: $status"
exit "$status"
