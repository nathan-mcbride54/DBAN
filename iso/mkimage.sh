#!/bin/bash
# Assemble the bootable DBAN ISO. Runs inside the builder container.
# Writes /out/dban.iso (bind-mounted from the host's dist/).
#
# x86_64 (DBAN_ARCH=x86_64): hybrid BIOS + UEFI image.
# arm64  (DBAN_ARCH=arm64):  UEFI-only image (ARM has no legacy BIOS).
set -euo pipefail

LABEL="${DBAN_LABEL:-DBAN}"
ARCH="${DBAN_ARCH:-x86_64}"
WORK=/work

if [ ! -d /out ]; then
    echo "FATAL: /out is not mounted — the host dist/ bind mount is missing." >&2
    echo "       On Windows, run via iso/build.ps1; on Git Bash, iso/build.sh." >&2
    exit 3
fi
ISOROOT="$WORK/isoroot"
INITRAMFS="$WORK/initramfs"

rm -rf "$WORK"
mkdir -p "$ISOROOT/boot/grub" "$INITRAMFS/bin" "$INITRAMFS/proc" \
         "$INITRAMFS/sys" "$INITRAMFS/dev" "$INITRAMFS/tmp"

# ---- initramfs: kernel's first and only userland ----
cp /build/dban "$INITRAMFS/bin/dban"
cp /build/init  "$INITRAMFS/init"
chmod +x "$INITRAMFS/init" "$INITRAMFS/bin/dban"
# A minimal /bin/sh fallback (busybox) only for the unexpected-exit panic path.
if [ -x /bin/busybox ]; then
    cp /bin/busybox "$INITRAMFS/bin/busybox"
    ln -sf busybox "$INITRAMFS/bin/sh"
    ln -sf busybox "$INITRAMFS/bin/mount"
    ln -sf busybox "$INITRAMFS/bin/echo"
fi

( cd "$INITRAMFS" && find . | cpio -o -H newc 2>/dev/null | gzip -9 ) \
    > "$ISOROOT/boot/initramfs.gz"

# ---- kernel ----
cp /boot/vmlinuz-lts "$ISOROOT/boot/vmlinuz"

# ---- bootloader config ----
# ARM consoles are commonly serial; offer it on tty1 and ttyAMA0/ttyS0.
if [ "$ARCH" = "arm64" ]; then
    CONSOLE="console=tty1 console=ttyAMA0,115200 console=ttyS0,115200"
else
    CONSOLE="console=tty1"
fi
cat > "$ISOROOT/boot/grub/grub.cfg" <<EOF
set timeout=3
set default=0
insmod all_video
menuentry "DBAN — secure disk eraser" {
    linux /boot/vmlinuz quiet loglevel=0 $CONSOLE
    initrd /boot/initramfs.gz
}
menuentry "DBAN (safe graphics / nomodeset)" {
    linux /boot/vmlinuz quiet loglevel=0 $CONSOLE nomodeset
    initrd /boot/initramfs.gz
}
EOF

# ---- per-arch UEFI GRUB binary + El Torito ESP ----
case "$ARCH" in
    arm64)
        EFI_FORMAT="arm64-efi"
        EFI_NAME="BOOTAA64.EFI"
        ;;
    *)
        EFI_FORMAT="x86_64-efi"
        EFI_NAME="BOOTX64.EFI"
        ;;
esac

mkdir -p "$WORK/efi/boot"
grub-mkstandalone \
    --format="$EFI_FORMAT" \
    --output="$WORK/efi/boot/$EFI_NAME" \
    --locales="" --fonts="" \
    "boot/grub/grub.cfg=$ISOROOT/boot/grub/grub.cfg"

# Size the FAT ESP to the actual GRUB EFI binary plus slack (a fixed 1.44M
# floppy overflows: "Disk full").
EFI_BYTES=$(stat -c %s "$WORK/efi/boot/$EFI_NAME")
ESP_MIB=$(( (EFI_BYTES / 1048576) + 2 ))
dd if=/dev/zero of="$WORK/efiboot.img" bs=1M count="$ESP_MIB" status=none
mkfs.vfat -n DBANEFI "$WORK/efiboot.img" >/dev/null
mmd   -i "$WORK/efiboot.img" ::/EFI ::/EFI/BOOT
mcopy -i "$WORK/efiboot.img" "$WORK/efi/boot/$EFI_NAME" "::/EFI/BOOT/$EFI_NAME"
cp "$WORK/efiboot.img" "$ISOROOT/boot/grub/efiboot.img"

# Also place the EFI binary in the ISO9660 tree so USB UEFI boot works with
# tools that look for /EFI/BOOT/BOOT*.EFI (Rufus, plain dd, Ventoy).
mkdir -p "$ISOROOT/EFI/BOOT"
cp "$WORK/efi/boot/$EFI_NAME" "$ISOROOT/EFI/BOOT/$EFI_NAME"

if [ "$ARCH" = "x86_64" ]; then
    # BIOS El Torito boot image via grub-pc-eltorito.
    grub-mkstandalone \
        --format=i386-pc \
        --output="$WORK/core.img" \
        --install-modules="linux normal iso9660 biosdisk search all_video gzio part_gpt part_msdos" \
        --modules="linux normal iso9660 biosdisk search" \
        --locales="" --fonts="" \
        "boot/grub/grub.cfg=$ISOROOT/boot/grub/grub.cfg"
    cat /usr/lib/grub/i386-pc/cdboot.img "$WORK/core.img" > "$ISOROOT/boot/grub/bios.img"

    # Hybrid: BIOS (El Torito + boot info table) and UEFI (alt boot) entries.
    xorriso -as mkisofs \
        -volid "$LABEL" \
        -o /out/dban.iso \
        -graft-points \
        -b boot/grub/bios.img \
            -no-emul-boot -boot-load-size 4 -boot-info-table \
            --grub2-boot-info \
        -eltorito-alt-boot \
        -e boot/grub/efiboot.img \
            -no-emul-boot \
        -isohybrid-gpt-basdat \
        -r -J "$ISOROOT"
else
    # UEFI-only image: a single EFI El Torito entry, GPT for USB boot.
    xorriso -as mkisofs \
        -volid "$LABEL" \
        -o /out/dban.iso \
        -graft-points \
        -e boot/grub/efiboot.img \
            -no-emul-boot \
        -isohybrid-gpt-basdat \
        -r -J "$ISOROOT"
fi

echo "ISO label: $LABEL  arch: $ARCH"
echo "Wrote /out/dban.iso"
