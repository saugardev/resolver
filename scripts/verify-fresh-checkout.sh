#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
fresh_root="$(mktemp -d "${TMPDIR:-/tmp}/livy-resolver-fresh.XXXXXX")"
fresh_checkout="$fresh_root/resolver"
mkdir -p "$fresh_checkout"

cleanup() {
  rm -rf -- "$fresh_root"
}
trap cleanup EXIT

git -C "$repo_root" archive --format=tar HEAD | tar -xf - -C "$fresh_checkout"

if grep -En '(\.\./livy-core|git[[:space:]]*=|livy-provenance-sdk)' "$fresh_checkout/Cargo.toml"; then
  echo "fresh checkout contains an external path or git dependency" >&2
  exit 1
fi

export CARGO_TARGET_DIR="$fresh_root/target"
cargo check --manifest-path "$fresh_checkout/Cargo.toml" --locked --all-targets --all-features
cargo test --manifest-path "$fresh_checkout/Cargo.toml" --locked --all-targets --all-features
