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
# Output: dist/dban.iso  (hybrid BIOS + UEFI, boots from CD or USB).
#
# Run from the repo root:  ./iso/build.sh
# Requires: Docker. Everything else happens inside the container.

set -euo pipefail

# On Git Bash / MSYS (Windows), the shell rewrites Unix-looking arguments such
# as the container-side `/out` mount target into Windows paths, which silently
# breaks the bind mount. Disable that just for this script.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL="*"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
IMAGE="dban-iso-builder"
LABEL="DBAN_$(date +%Y%m%d)"

echo ">> Building ISO builder image..."
# Use a relative build context from the repo root. With MSYS_NO_PATHCONV set,
# an absolute MSYS path like /d/DBAN is not a valid client-side path for
# `docker build`, whereas `.` is unambiguous on every platform.
( cd "$ROOT" && docker build -t "$IMAGE" -f iso/Dockerfile . )

echo ">> Producing ISO inside container..."
mkdir -p "$ROOT/dist"
docker run --rm \
    -e DBAN_LABEL="$LABEL" \
    -v "$ROOT/dist:/out" \
    "$IMAGE"

echo ">> Done. Image at dist/dban.iso"
ls -lh "$ROOT/dist/dban.iso"
