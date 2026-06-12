# Scour ISO builder.
#
# Stage 1 compiles the TUI to a fully static musl binary (no libc runtime
# dependency, so it can be PID 1 in an otherwise-empty initramfs).
# Stage 2 assembles a hybrid BIOS+UEFI ISO around the Alpine kernel.

# ---------------------------------------------------------------------------
# Stage 1 — static binary
# ---------------------------------------------------------------------------
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev file
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates

# x86_64 static target. The binary must not need a dynamic loader: as PID 1 in
# an otherwise-empty initramfs there is no ld.so to resolve it. `file` reports
# musl static binaries as "statically linked" (or "static-pie linked"); a
# dynamic build would say "dynamically linked", which we reject.
RUN rustup target add x86_64-unknown-linux-musl \
 && cargo build --release --bin scour --target x86_64-unknown-linux-musl \
 && strip target/x86_64-unknown-linux-musl/release/scour \
 && cp target/x86_64-unknown-linux-musl/release/scour /scour \
 && file /scour \
 && file /scour | grep -qi 'static' \
      || (echo "FATAL: scour is not statically linked" >&2; exit 1)

# ---------------------------------------------------------------------------
# Stage 2 — assemble the ISO
# ---------------------------------------------------------------------------
FROM alpine:3.20 AS iso

# Kernel + firmware + the tools to make a bootable hybrid image.
RUN apk add --no-cache \
        linux-lts \
        linux-firmware-none \
        xorriso \
        grub \
        grub-bios \
        grub-efi \
        mtools \
        dosfstools \
        cpio \
        gzip \
        bash

COPY --from=builder /scour /build/scour
COPY iso/init /build/init
COPY iso/mkimage.sh /build/mkimage.sh
RUN chmod +x /build/init /build/mkimage.sh

ENV SCOUR_LABEL=SCOUR
ENTRYPOINT ["/build/mkimage.sh"]
