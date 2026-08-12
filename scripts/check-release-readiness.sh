#!/usr/bin/env bash
set -uo pipefail

cd "$(dirname "$0")/.."

failures=0

pass() {
  printf 'PASS: %s\n' "$1"
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  failures=$((failures + 1))
}

require_fixed() {
  local needle=$1
  local file=$2
  local label=$3
  if grep -F -q -- "$needle" "$file"; then
    pass "$label"
  else
    fail "$label"
  fi
}

deny_fixed() {
  local needle=$1
  shift
  local label=$1
  shift
  if grep -R -F -q -- "$needle" "$@"; then
    fail "$label"
    grep -R -n -F -- "$needle" "$@" >&2 || true
  else
    pass "$label"
  fi
}

require_fixed 'version = "0.3.0"' Cargo.toml 'crate version is 0.3.0'
require_fixed 'default = ["usb", "ble"]' Cargo.toml 'default features exclude test-mode'
require_fixed '[package.metadata.docs.rs]' Cargo.toml 'docs.rs package metadata exists'
require_fixed 'all-features = true' Cargo.toml 'docs.rs builds opt-in APIs'
require_fixed 'reai-board-sdk = "0.3"' README.md 'English quick start uses 0.3'
require_fixed 'reai-board-sdk = "0.3"' README.zh-CN.md 'Chinese quick start uses 0.3'
require_fixed '`test-mode` is opt-in' README.md 'English README explains opt-in test-mode'
require_fixed 'test-mode` 默认不启用' README.zh-CN.md 'Chinese README explains opt-in test-mode'
require_fixed '/v0.3.0/assets/' README.md 'English assets use immutable v0.3.0 tag'
require_fixed '/v0.3.0/assets/' README.zh-CN.md 'Chinese assets use immutable v0.3.0 tag'

deny_fixed \
  'ReAI Vibe Board' \
  'public SDK product surfaces use ReAI-Vibe-Board' \
  Cargo.toml README.md README.zh-CN.md src/lib.rs examples

usb_tree=$(cargo tree -p reai-board-sdk -e normal --prefix none \
  --no-default-features --features usb)
if grep -F -q -- 'msbc-decoder ' <<<"$usb_tree"; then
  fail 'USB-only dependency tree excludes LGPL msbc-decoder'
else
  pass 'USB-only dependency tree excludes LGPL msbc-decoder'
fi

if (( failures > 0 )); then
  printf '%d release-readiness check(s) failed\n' "$failures" >&2
  exit 1
fi

printf 'All release-readiness checks passed.\n'
