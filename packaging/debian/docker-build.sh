#!/bin/sh
# Build the release binary + .deb inside a Debian 12 (bookworm) container so the result runs on
# Debian 12/13, Ubuntu 22.04+ (glibc >= 2.36). Needs docker. Output: dist/
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$ROOT/dist"
docker run --rm -v "$ROOT":/src:ro -v "$ROOT/dist":/out debian:12-slim sh -ec '
  apt-get update -qq && apt-get install -y -qq --no-install-recommends curl ca-certificates gcc libc6-dev dpkg-dev pkg-config libgtk-4-dev >/dev/null
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
  . $HOME/.cargo/env
  mkdir /build && cd /src && tar --exclude=./target --exclude=./dist --exclude=./.git --exclude=./packaging/arch/src --exclude=./packaging/arch/pkg --exclude="*.pkg.tar.*" --exclude="*.deb" -cf - . | tar -xf - -C /build && cd /build
  # Headless daemon build (verifies zero GUI dependencies)
  cargo build --locked --release
  cargo test --locked --release --lib
  # Optional GTK4 dashboard popup
  cargo build --locked --release --features popup --bin svmonitor-popup
  OUT=/out sh packaging/debian/build-deb.sh /build/target/release/svmonitor amd64 /build/target/release/svmonitor-popup
  ldd --version | head -1
  ldd target/release/svmonitor
  ldd target/release/svmonitor-popup
  objdump -T target/release/svmonitor 2>/dev/null | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -1
'
