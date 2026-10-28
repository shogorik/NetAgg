#!/usr/bin/env bash
# Builds NetAgg's Linux packages on Ubuntu 24.04 (the oldest it supports, so glibc matches):
# the desktop net-agg .deb (`tauri build`), then netagg-server and netagg-apt (scripts/build-server-deb.sh).
# Everything lands in src-tauri/target/release/bundle/deb/; sign it on Windows with
# `scripts/release.sh --linux-from <that folder>`.
#
#   scripts/build-linux.sh
set -euo pipefail

cd "$(dirname "$0")/.."
# Panic messages carry source paths: keep the build machine's home folder out of the binaries.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build"
rm -rf src-tauri/target/release/bundle/deb
npm run tauri build -- --bundles deb
scripts/build-server-deb.sh
