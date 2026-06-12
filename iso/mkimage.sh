#!/bin/bash
# Assemble the hybrid BIOS+UEFI ISO. Runs inside the builder container.
# Writes /out/scour.iso (bind-mounted from the host's dist/).
set -euo pipefail

LABEL="${SCOUR_LABEL:-SCOUR}"
WORK=/work
ISOROOT="$WORK/isoroot"
INITRAMFS="$WORK/initramfs"

rm -rf "$WORK"
mkdir -p "$ISOROOT/boot/grub" "$INITRAMFS/bin" "$INITRAMFS/proc" \
         "$INITRAMFS/sys" "$INITRAMFS/dev" "$INITRAMFS/tmp"

# ---- initramfs: kernel's first and only userland ----
cp /build/scour "$INITRAMFS/bin/scour"
cp /build/init  "$INITRAMFS/init"
chmod +x "$INITRAMFS/init" "$INITRAMFS/bin/scour"
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
# quiet + console on tty1; scour owns the screen.
cat > "$ISOROOT/boot/grub/grub.cfg" <<EOF
set timeout=3
set default=0
insmod all_video
menuentry "Scour — secure disk eraser" {
    linux /boot/vmlinuz quiet loglevel=0 console=tty1
    initrd /boot/initramfs.gz
}
menuentry "Scour (safe graphics / nomodeset)" {
    linux /boot/vmlinuz quiet loglevel=0 console=tty1 nomodeset
    initrd /boot/initramfs.gz
}
EOF

# ---- hybrid ISO (BIOS via grub-pc-eltorito, UEFI via an EFI boot image) ----
grub-mkstandalone \
    --format=i386-pc \
    --output="$WORK/core.img" \
    --install-modules="linux normal iso9660 biosdisk search all_video gzio part_gpt part_msdos" \
    --modules="linux normal iso9660 biosdisk search" \
    --locales="" --fonts="" \
    "boot/grub/grub.cfg=$ISOROOT/boot/grub/grub.cfg"
cat /usr/lib/grub/i386-pc/cdboot.img "$WORK/core.img" > "$ISOROOT/boot/grub/bios.img"

# UEFI El Torito image (FAT) with a GRUB EFI binary.
mkdir -p "$WORK/efi/boot"
grub-mkstandalone \
    --format=x86_64-efi \
    --output="$WORK/efi/boot/bootx64.efi" \
    --locales="" --fonts="" \
    "boot/grub/grub.cfg=$ISOROOT/boot/grub/grub.cfg"

mformat -i "$WORK/efiboot.img" -C -f 1440 -N 0 ::  2>/dev/null || \
    dd if=/dev/zero of="$WORK/efiboot.img" bs=1k count=1440
mformat -i "$WORK/efiboot.img" ::
mmd     -i "$WORK/efiboot.img" ::/EFI ::/EFI/BOOT
mcopy   -i "$WORK/efiboot.img" "$WORK/efi/boot/bootx64.efi" ::/EFI/BOOT/BOOTX64.EFI
cp "$WORK/efiboot.img" "$ISOROOT/boot/grub/efiboot.img"

xorriso -as mkisofs \
    -volid "$LABEL" \
    -o /out/scour.iso \
    -graft-points \
    -b boot/grub/bios.img \
        -no-emul-boot -boot-load-size 4 -boot-info-table \
        --grub2-boot-info \
    -eltorito-alt-boot \
    -e boot/grub/efiboot.img \
        -no-emul-boot \
    -isohybrid-gpt-basdat \
    -r -J "$ISOROOT"

echo "ISO label: $LABEL"
echo "Wrote /out/scour.iso"
