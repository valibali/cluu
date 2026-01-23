/*
 * CLUU C Runtime Startup (crt0.S)
 *
 * This is the entry point for C programs compiled with newlib on CLUU.
 * It performs the following:
 *   1. Clear the BSS section
 *   2. Initialize the CLUU runtime (heap, fd table)
 *   3. Call main(argc, argv, envp)
 *   4. Call _exit with the return value
 *
 * Memory layout (from user.ld):
 *   0x00400000 - Code (.text)
 *   0x00500000+ - Data (.rodata, .data, .bss)
 *   0x00800000 - Dynamic heap start
 *   0x40000000 - Heap maximum
 *   0x7F000000 - Stack bottom (grows down)
 *   0x80000000 - Stack top
 */

.section .text._start
.global _start
.type _start, @function

_start:
    /* Clear BSS section to zero */
    lea     __bss_start(%rip), %rdi     /* destination = BSS start */
    lea     __bss_end(%rip), %rcx       /* end address */
    sub     %rdi, %rcx                   /* count = end - start */
    test    %rcx, %rcx                   /* if count == 0, skip */
    jz      .Lbss_done
    xor     %eax, %eax                   /* fill with zero */
    rep     stosb                        /* memset(bss, 0, count) */
.Lbss_done:

    /*
     * Initialize CLUU runtime.
     * This sets up:
     *   - Heap allocator
     *   - File descriptor table (fd 0-3 = stdin/stdout/stderr/stdlog)
     */
    call    __cluu_init

    /*
     * Call main(argc, argv, envp)
     *
     * In CLUU, we don't have traditional command-line arguments yet.
     * Pass argc=0, argv=NULL, envp=NULL for now.
     * TODO: Parse arguments from ProcessInfo or IPC.
     */
    xor     %edi, %edi                   /* argc = 0 */
    xor     %esi, %esi                   /* argv = NULL */
    xor     %edx, %edx                   /* envp = NULL */
    call    main

    /*
     * Exit with main's return value.
     * main() returns int in %eax, pass it to _exit.
     */
    mov     %eax, %edi                   /* status = return value */
    call    _exit

    /* Should never reach here, but loop just in case */
.Lhang:
    hlt
    jmp     .Lhang

.size _start, . - _start

/*
 * Weak symbol for __cluu_init if not provided.
 * This allows linking without the full runtime for testing.
 */
.weak __cluu_init
__cluu_init:
    ret
