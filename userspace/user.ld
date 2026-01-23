/* CLUU Userspace Linker Script */
/* Memory layout:
 *   0x00400000 - Code (.text)
 *   0x00500000+ - Data (.rodata, .data, .bss)
 *   0x00800000 - Dynamic heap start (grows up via syscalls)
 *   0x40000000 - Heap maximum
 *   0x7F000000 - Stack bottom (grows down)
 *   0x80000000 - Stack top
 */

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

    /* Note: Heap is now dynamically allocated at 0x800000 via syscalls.
     * No static reservation needed - the kernel's space_map_range handles it.
     * Maximum heap extends to 0x40000000 (1GB).
     */

    /* Stack grows down from 0x80000000 (2GB) with 16MB size */
    . = 0x80000000;
    __stack_top = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
