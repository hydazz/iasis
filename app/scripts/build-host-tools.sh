#!/bin/sh
# Build iasisd and iasis for the app to carry.

set -eu

root="$(cd "${SRCROOT}/.." && pwd)"
out="${DERIVED_FILE_DIR}/HostTools"

# Xcode's environment has neither mise nor cargo on its PATH.
PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"

cd "${root}"
if command -v mise >/dev/null 2>&1; then
    mise exec -- cargo build --release --locked --package iasis
else
    cargo build --release --locked --package iasis
fi

mkdir -p "${out}"
cp target/release/iasisd target/release/iasis "${out}/"
