# DBAN ISO builder (multi-arch).
#
# Stage 1 compiles the TUI to a fully static musl binary (no libc runtime
# dependency, so it can be PID 1 in an otherwise-empty initramfs).
# Stage 2 assembles the bootable ISO around the Alpine kernel: a hybrid
# BIOS+UEFI image on x86_64, a UEFI-only image on arm64.
#
# Build args (set by iso/build.sh / iso/build.ps1):
#   RUST_TARGET   musl target triple (default x86_64-unknown-linux-musl)
#   DBAN_ARCH     x86_64 | arm64   (selects BIOS+UEFI vs UEFI-only assembly)
# Use `--platform linux/amd64` or `linux/arm64` so the right-arch kernel and
# GRUB packages are pulled.

# ---------------------------------------------------------------------------
# Stage 1 — static binary
# ---------------------------------------------------------------------------
FROM rust:1-alpine AS builder

ARG RUST_TARGET=x86_64-unknown-linux-musl

RUN apk add --no-cache musl-dev file
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates

# Fully static target. The binary must not need a dynamic loader: as PID 1 in
# an otherwise-empty initramfs there is no ld.so to resolve it. `file` reports
# musl static binaries as "statically linked" (or "static-pie linked"); a
# dynamic build would say "dynamically linked", which we reject.
RUN rustup target add "$RUST_TARGET" \
 && cargo build --release --bin dban --target "$RUST_TARGET" \
 && strip "target/$RUST_TARGET/release/dban" \
 && cp "target/$RUST_TARGET/release/dban" /dban \
 && file /dban \
 && file /dban | grep -qi 'static' \
      || (echo "FATAL: dban is not statically linked" >&2; exit 1)

# ---------------------------------------------------------------------------
# Stage 2 — assemble the ISO
# ---------------------------------------------------------------------------
FROM alpine:3.20 AS iso

ARG DBAN_ARCH=x86_64

# Kernel + firmware + the tools to make a bootable image. grub-bios only exists
# on x86 (no BIOS boot on arm64), so it is added only for x86_64.
#
# The `grub` package's post-install trigger runs grub-probe against the build
# overlay and fails harmlessly in a container ("failed to get canonical path of
# overlay"); we never grub-install here — only grub-mkstandalone — so that error
# is tolerated, and the tools we actually need are verified afterwards.
RUN set -u; \
    PKGS="linux-lts linux-firmware-none xorriso grub grub-efi mtools dosfstools cpio gzip bash"; \
    if [ "$DBAN_ARCH" = "x86_64" ]; then PKGS="$PKGS grub-bios"; fi; \
    apk add --no-cache $PKGS || true; \
    command -v xorriso >/dev/null && command -v grub-mkstandalone >/dev/null \
      && command -v mkfs.vfat >/dev/null && test -f /boot/vmlinuz-lts \
      || (echo "FATAL: required ISO tooling missing" >&2; exit 1); \
    if [ "$DBAN_ARCH" = "x86_64" ]; then test -f /usr/lib/grub/i386-pc/cdboot.img \
      || (echo "FATAL: grub-bios missing" >&2; exit 1); fi

COPY --from=builder /dban /build/dban
COPY iso/init /build/init
COPY iso/mkimage.sh /build/mkimage.sh
RUN chmod +x /build/init /build/mkimage.sh

ENV DBAN_LABEL=DBAN
ENV DBAN_ARCH=$DBAN_ARCH
ENTRYPOINT ["/build/mkimage.sh"]
