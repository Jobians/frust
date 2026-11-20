#!/usr/bin/env bash
# Thread-local key counter for Android shared objects.
#
# On aarch64-linux-android, Rust's std has no native TLS: every `thread_local!` static takes one
# pthread key (pthread_key_create) the first time a thread touches it, and bionic never frees keys.
# bionic allows PTHREAD_KEYS_MAX (128) keys per process, so a library that links too many
# thread-local statics aborts with "out of TLS keys" once the process runs out.
#
# This script counts the distinct thread-local statics linked into an unstripped library: the
# distinct `__rust_std_internal_init_fn` paths named by `std::sys::thread_local::os::Storage<..>`
# instantiations in `llvm-nm -C --defined-only` output, plus a per-crate breakdown.
#
# The count is an UPPER BOUND on keys taken by this library: a static only takes a key when first
# used on some thread. It also ignores non-Rust code in the process (the Java runtime, the C
# libraries, other native libraries), which takes keys from the same budget of PTHREAD_KEYS_MAX.
#
# Usage: scripts/ci/tls-key-count.sh <path-to-unstripped-android-.so> [budget]
#   With a budget, exits 1 when the count exceeds it.
#
# llvm-nm is found under $ANDROID_NDK_HOME/toolchains/llvm/prebuilt/*/bin, else on PATH.
#
# Exit codes: 0 ok (within budget or no budget), 1 count exceeds budget, 2 usage or tool error.
set -euo pipefail

usage() {
  echo "usage: $0 <path-to-unstripped-android-.so> [budget]" >&2
  exit 2
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
lib=$1
budget=${2:-}
[ -f "$lib" ] || { echo "error: not a file: $lib" >&2; exit 2; }
if [ -n "$budget" ]; then
  case "$budget" in
    '' | *[!0-9]*) echo "error: budget must be a non-negative integer: $budget" >&2; exit 2 ;;
  esac
fi

nm=""
if [ -n "${ANDROID_NDK_HOME:-}" ]; then
  for cand in "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-nm; do
    if [ -x "$cand" ]; then
      nm=$cand
      break
    fi
  done
fi
if [ -z "$nm" ] && command -v llvm-nm >/dev/null 2>&1; then
  nm=$(command -v llvm-nm)
fi
if [ -z "$nm" ]; then
  echo "error: llvm-nm not found (set ANDROID_NDK_HOME or put llvm-nm on PATH)" >&2
  exit 2
fi

# llvm-nm runs on its own first: in the pipeline below its failure would look like "no match".
# A library with no symbols at all (a stripped one) is an input error too, not a count of 0.
symbols=$(mktemp)
trap 'rm -f "$symbols"' EXIT
if ! "$nm" -C --defined-only "$lib" >"$symbols"; then
  echo "error: llvm-nm failed on $lib (not an object file?)" >&2
  exit 2
fi
if [ ! -s "$symbols" ]; then
  echo "error: llvm-nm found no symbols in $lib (stripped?)" >&2
  exit 2
fi

# One path per line: the type argument naming each static's init function. Only grep's "no
# match" can fail here, and that is a count of 0.
paths=$(grep 'sys::thread_local::os::Storage' "$symbols" \
  | grep '__rust_std_internal_init_fn' \
  | grep -oE '::(try_initialize|get)::<.+::__rust_std_internal_init_fn>' \
  | sed -E 's/^::(try_initialize|get)::<//; s/::__rust_std_internal_init_fn>$//' \
  | sort -u || true)

if [ -z "$paths" ]; then
  count=0
else
  count=$(printf '%s\n' "$paths" | wc -l | tr -d ' ')
fi

echo "distinct thread-local statics: $count"
if [ "$count" -gt 0 ]; then
  # Crate = first path segment, ignoring a leading '<' of qualified paths.
  printf '%s\n' "$paths" \
    | sed -E 's/^<//; s/(::|<).*$//' \
    | sort | uniq -c | sort -rn \
    | awk '{ printf "%5d %s\n", $1, $2 }'
fi

if [ -n "$budget" ] && [ "$count" -gt "$budget" ]; then
  echo "error: $count thread-local statics exceed the budget of $budget" >&2
  exit 1
fi
