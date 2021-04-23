BOOTBOOT Example Bootable Disk Images
=====================================

See [BOOTBOOT Protocol](https://gitlab.com/bztsrc/bootboot) for common details.

- disk-rpi.img.gz: an example image for AArch64 and RaspberryPi 3 and 4
- disk-x86.img.gz: an example image for x86_64 (CDROM, BIOS, UEFI)
- initrd.rom.gz: an example initrd ROM image (for embedded BIOS systems)
- coreboot-x86.rom.gz: an example coreboot ROM image with BOOTBOOT payload for PC

Before you can use the images, uncompress them with `gzip -d`. I've used [mkbootimg](https://gitlab.com/bztsrc/bootboot/tree/master/mkbootimg)
to generate these images, and I recommend [USBImager](https://bztsrc.gitlab.io/usbimager) or `dd` to write them to physical disks.

The disk-x86.img is a special hybrid image, which can be renamed to disk-x86.iso and then burnt to a CDROM; it can also be
booted from an USB stick in a BIOS machine as well as in an UEFI machine.

The disk-rpi.img can be written to an SDCard (Class 10) and booted on a Raspberry Pi 3 and 4.

The disk images contain only one boot partition. Feel free to use `fdisk` and add more partitions to your needs, or
modify mkbootimg.json and add more elements to the `partitions` array.

Compilation
-----------

First, move this `images` directory to your local master branch of the repository.
See mkbootimg.json. Look at the beginning of the Makefile too, you'll find configurable variables there.

- PLATFORM: either "x86" or "rpi", this selects which disk image to create
- OVMF: path to the EFI firmware image

Then just run `make`.

Compiling coreboot-*.rom requires a [coreboot build environment](https://gitlab.com/bztsrc/bootboot/tree/master/x86_64-cb).

Testing
-------

To test BOOTBOOT in qemu, you can use:
```
make rom
```
Will boot the example kernel from ROM (via BIOS Boot Spec, diskless test).
```
make bios
```
Will boot the example kernel from disk (using BIOS).
```
make cdrom
```
Will boot the example kernel in El Torito "no emulation" mode (BIOS).
```
make efi
```
Will boot the example kernel from disk using UEFI. You must provide your own TianoCore image, and set the path for it in the Makefile.
```
make eficdrom
```
Will boot the example kernel under UEFI from CDROM.
```
make grubcdrom
```
Will create a cdrom image using grub-mkrescue and boot BOOTBOOT using Multiboot.
```
make linux
```
Will boot the example kernel by booting BOOTBOOT via the [Linux/x86 Boot Protocol](https://www.kernel.org/doc/html/latest/x86/boot.html).
```
make sdcard
```
Will boot the example kernel from SDCard emulating "raspi3" machine under qemu (requires qemu-system-aarch64).
```
make coreboot
```
To test BOOTBOOT as a coreboot payload (no BIOS, no UEFI). With PLATFORM=x86 emulates a PC, and ARM64 otherwise.
```
make bochs
```
Testing with bochs (using BIOS).
