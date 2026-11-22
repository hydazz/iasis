#!/bin/sh
# Build the app’s host tools.

set -eu

root="$(cd "${SRCROOT}/.." && pwd)"
out="${DERIVED_FILE_DIR}/HostTools"

# Xcode's environment has neither mise nor cargo on its PATH.
PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"

# Link the gateway to the libpcap supplied by macOS.
export LIBPCAP_NO_PKG_CONFIG=1

cd "${root}"
if command -v mise >/dev/null 2>&1; then
    mise exec -- cargo build --release --locked --package iasis
else
    cargo build --release --locked --package iasis
fi

mkdir -p "${out}"
cp target/release/iasisd target/release/iasis target/release/iasis-gateway "${out}/"
