/* DOOM-specific linker script — based on userspace/user.ld.
 *
 * Fixes PAGE_FAULT during DG_Init: the original user.ld creates a separate
 * read-only PT_LOAD for .rodata.  SDL2's initialization path writes to
 * addresses that the compiler placed in .rodata (function pointer tables
 * that are logically mutable after relocation).  The VFS ELF loader maps
 * .rodata as read-only (MAP_SHARE_PHYS), so the write faults.
 *
 * Fix: merge .rodata into the .data output section so it lands in the RW
 * PT_LOAD segment.  This is the pragmatic fix — the deeper issue is that
 * the VFS demand-zero page handler can map a wrong page when .bss and
 * .rodata share a page boundary, but that is outside userspace/doom-cluu/
 * scope.  Merging .rodata into .data is safe because DOOM is a single
 * process with no fork/COW semantics that depend on .rodata being
 * read-only.  Also places .init_array, .fini_array, .data.rel.ro, .ctors,
 * .dtors, .got in .data to prevent page-sharing with text or rodata. */

ENTRY(_start)

SECTIONS
{
    . = 0x400000;

    .text : ALIGN(4K)
    {
        *(.text._start)
        *(.text .text.*)
    }

    .data : ALIGN(4K)
    {
        *(.rodata .rodata.*)
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.init_array .init_array.*)
        *(.fini_array .fini_array.*)
        *(.ctors .ctors.*)
        *(.dtors .dtors.*)
        *(.got .got.*)
    }

    .tdata : ALIGN(16)
    {
        __tdata_start = .;
        *(.tdata .tdata.*)
        __tdata_end = .;
    }

    .tbss : ALIGN(16)
    {
        __tbss_start = .;
        *(.tbss .tbss.*)
        __tbss_end = .;
    }

    .bss : ALIGN(4K)
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    . = 0x80000000;
    __stack_top = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
