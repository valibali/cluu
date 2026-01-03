/* CLUU Userspace Linker Script */
/* Links userspace programs starting at 0x400000 */

ENTRY(_start)

SECTIONS
{
    /* Userspace programs start at 0x400000 (4MB) */
    . = 0x400000;

    .text : ALIGN(4K)
    {
        *(.text._start)   /* Entry point first */
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
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    /* Heap area starts after BSS */
    . = ALIGN(4K);
    __heap_start = .;
    . += 4M;  /* 4MB heap */
    __heap_end = .;

    /* Stack grows down from 0x800000 (8MB) */
    . = 0x800000;
    __stack_top = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
