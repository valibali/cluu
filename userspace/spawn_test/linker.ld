/*
 * Userspace Linker Script for CLUU
 *
 * Memory layout:
 * 0x00000000 - 0x00400000  Reserved (NULL pointer protection)
 * 0x00400000 - 0x00600000  Text segment (code, read+execute)
 * 0x00600000 - 0x00800000  Data/BSS segment (data, read+write)
 * 0x00800000 - 0x40000000  Heap (grows up via _sbrk)
 * 0x7ff00000 - 0x80000000  Stack (grows down, 16 MB)
 */

OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

SECTIONS
{
    /* Text segment starts at 0x400000 (standard for x86_64 userspace) */
    . = 0x400000;

    .text : ALIGN(4K) {
        *(.text._start)     /* _start must be first */
        *(.text.*)
        *(.text)
    }

    /* Read-only data */
    .rodata : ALIGN(4K) {
        *(.rodata.*)
        *(.rodata)
    }

    /* Data segment starts at 0x600000 */
    . = 0x600000;

    .data : ALIGN(4K) {
        *(.data.*)
        *(.data)
    }

    .bss : ALIGN(4K) {
        *(COMMON)
        *(.bss.*)
        *(.bss)
    }

    /* Heap starts at 0x800000 (will be managed by brk syscall) */
    . = ALIGN(4K);
    _heap_start = .;

    /* Discard debug info for smaller binary */
    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}
