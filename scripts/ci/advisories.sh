#!/usr/bin/env bash
# Run `cargo deny check advisories` against every tracked Cargo.lock, using the
# root deny.toml for all of them. The root workspace and each standalone
# workspace (excluded from the root Cargo.toml) have their own lockfile, so a
# single run from the root would miss the standalone ones.
#
# No `--config` flag is passed: its position differs between cargo-deny
# releases (after `check` up to 0.19, before the subcommand from 0.20).
# cargo-deny finds deny.toml by walking up from the manifest's directory, so
# every workspace resolves to the root file as long as none has its own. A
# nested deny.toml would replace the root ignore list for that workspace
# without any sign here, so the script refuses to run if one is tracked.
#
# Usage: scripts/ci/advisories.sh   (run from anywhere; it cd's to the repo root)
# Exits non-zero if any workspace fails, after listing the failed workspaces.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

lockfiles=()
while IFS= read -r lock; do
  lockfiles+=("$lock")
done < <(git ls-files '*Cargo.lock')

if [ "${#lockfiles[@]}" -eq 0 ]; then
  echo "error: no tracked Cargo.lock found" >&2
  exit 1
fi

nested="$(git ls-files 'deny.toml' '.deny.toml' '*/deny.toml' '*/.deny.toml' | grep -vx 'deny.toml' || true)"
if [ -n "$nested" ]; then
  echo "error: a nested cargo-deny config would override the root deny.toml:" >&2
  while IFS= read -r path; do echo "  $path" >&2; done <<<"$nested"
  exit 1
fi

failed=()
for lock in "${lockfiles[@]}"; do
  dir="$(dirname "$lock")"
  if [ ! -f "$dir/Cargo.toml" ]; then
    echo "error: $lock has no $dir/Cargo.toml next to it" >&2
    failed+=("$dir (no Cargo.toml)")
    continue
  fi
  echo "=== advisories: $dir ==="
  if ! cargo deny --manifest-path "$dir/Cargo.toml" check advisories; then
    failed+=("$dir")
  fi
done

echo
if [ "${#failed[@]}" -gt 0 ]; then
  echo "advisories failed in ${#failed[@]} of ${#lockfiles[@]} workspaces:" >&2
  printf '  %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "advisories ok in all ${#lockfiles[@]} workspaces"
