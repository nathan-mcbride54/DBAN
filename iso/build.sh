#!/usr/bin/env bash
#
# Build the DBAN live ISO.
#
# Strategy: the smallest honest "purpose-built OS" that can actually talk to
# real storage controllers is the Linux kernel + our static Rust binary as
# PID 1. We take *only* the kernel and firmware from Alpine; there is no
# BusyBox, no shell, no distro userland. The initramfs contains exactly one
# program — `dban` — which the kernel launches as init.
#
# Output: dist/dban.iso  (x86_64: hybrid BIOS + UEFI; arm64: UEFI-only).
#
# Usage:   ./iso/build.sh [x86_64|arm64]
# Requires: Docker. Everything else happens inside the container.

set -euo pipefail

# On Git Bash / MSYS (Windows), the shell rewrites Unix-looking arguments such
# as the container-side `/out` mount target into Windows paths, which silently
# breaks the bind mount. Disable that just for this script.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL="*"

ARCH="${1:-${DBAN_ARCH:-x86_64}}"
case "$ARCH" in
    x86_64) RUST_TARGET="x86_64-unknown-linux-musl";  PLATFORM="linux/amd64" ;;
    arm64)  RUST_TARGET="aarch64-unknown-linux-musl"; PLATFORM="linux/arm64" ;;
    *) echo "unknown arch '$ARCH' (use x86_64 or arm64)" >&2; exit 2 ;;
esac

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
IMAGE="dban-iso-builder-$ARCH"
LABEL="DBAN_$(date +%Y%m%d)"

echo ">> Building ISO builder image ($ARCH)..."
# Use a relative build context from the repo root. With MSYS_NO_PATHCONV set,
# an absolute MSYS path like /d/DBAN is not a valid client-side path for
# `docker build`, whereas `.` is unambiguous on every platform.
( cd "$ROOT" && docker build \
    --platform "$PLATFORM" \
    --build-arg "DBAN_ARCH=$ARCH" \
    --build-arg "RUST_TARGET=$RUST_TARGET" \
    -t "$IMAGE" -f iso/Dockerfile . )

echo ">> Producing ISO inside container..."
mkdir -p "$ROOT/dist"
docker run --rm \
    --platform "$PLATFORM" \
    -e DBAN_LABEL="$LABEL" \
    -e DBAN_ARCH="$ARCH" \
    -v "$ROOT/dist:/out" \
    "$IMAGE"

echo ">> Done. Image at dist/dban.iso"
ls -lh "$ROOT/dist/dban.iso"
