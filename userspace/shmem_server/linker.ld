/* Linker script for CLUU userspace shell */

ENTRY(_start)

SECTIONS
{
    /* Start at 4MB for userspace */
    . = 0x400000;

    .text : ALIGN(4K)
    {
        *(.text.entry)
        *(.text .text.*)
    }

    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(4K)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /* Discard unneeded sections */
    /DISCARD/ :
    {
        *(.eh_frame)
        *(.note*)
        *(.comment)
    }
}
