.PHONY: all clean qemu

all:
	@make -C ./kernel all
	@make -C ./bootboot_images all

clean:
	@make -C ./kernel clean
	@make -C ./mkbootimg clean
	@make -C ./bootboot_images clean

qemu: all
	@make -C ./bootboot_images uefi
