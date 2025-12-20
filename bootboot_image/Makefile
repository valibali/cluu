PLATFORM=x86

# the path to OVMF.fd (for testing with EFI)
OVMF=/usr/share/ovmf/OVMF.fd

all: ../utilies/mkbootimg/mkbootimg initdir disk

# compile the image creator
../utilies/mkbootimg/mkbootimg:
	@make -C ../utilies/mkbootimg all

# create an initial ram disk image with the kernel inside
initdir:
	@mkdir initrd initrd/sys 2>/dev/null | true
	cp ../kernel/cluu-kernel-rust.x86_64.elf initrd/sys/core

# create hybrid disk / cdrom image or ROM image
disk: ../utilies/mkbootimg/mkbootimg initdir mkbootimg.json
	../utilies/mkbootimg/mkbootimg mkbootimg.json disk-$(PLATFORM).img
	@rm -rf initrd/sys initrd/bin

uefi:
	qemu-system-x86_64 -s -S -bios $(OVMF) -m 64 -drive file=disk-x86.img,format=raw -serial stdio -serial telnet:localhost:4321,server,nowait
	@printf '\033[0m'

uefi_nodebug:
	qemu-system-x86_64 -bios $(OVMF) -m 64 -drive file=disk-x86.img,format=raw -serial stdio
	@printf '\033[0m'

# clean up
clean:
	rm -rf initrd/sys initrd/bin *.bin *.img *.rom *.iso 2>/dev/null || true
