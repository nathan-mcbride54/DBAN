#!/usr/bin/env bash
#
# Build the Scour live ISO.
#
# Strategy: the smallest honest "purpose-built OS" that can actually talk to
# real storage controllers is the Linux kernel + our static Rust binary as
# PID 1. We take *only* the kernel and firmware from Alpine; there is no
# BusyBox, no shell, no distro userland. The initramfs contains exactly one
# program — `scour` — which the kernel launches as init.
#
# Output: dist/scour.iso  (hybrid BIOS + UEFI, boots from CD or USB).
#
# Run from the repo root:  ./iso/build.sh
# Requires: Docker. Everything else happens inside the container.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
IMAGE="scour-iso-builder"
LABEL="SCOUR_$(date +%Y%m%d)"

echo ">> Building ISO builder image..."
docker build -t "$IMAGE" -f "$HERE/Dockerfile" "$ROOT"

echo ">> Producing ISO inside container..."
mkdir -p "$ROOT/dist"
docker run --rm \
    -e SCOUR_LABEL="$LABEL" \
    -v "$ROOT/dist:/out" \
    "$IMAGE"

echo ">> Done. Image at dist/scour.iso"
ls -lh "$ROOT/dist/scour.iso"
