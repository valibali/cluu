.PHONY: all clean qemu

all:
	@make -C ./kernel all
	@make -C ./bootboot_image all

clean:
	@make -C ./kernel clean
	@make -C ./utilies/mkbootimg clean
	@make -C ./bootboot_image clean

qemu: clean all
	@make -C ./bootboot_image uefi

doc: clean
	@make -C ./kernel doc

qemu_nodebug: all
	@make -C ./bootboot_image uefi_nodebug
