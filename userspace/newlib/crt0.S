/*
 * CLUU C Runtime Startup (crt0.S)
 *
 * This is the entry point for C programs compiled with newlib on CLUU.
 * It performs the following:
 *   1. Clear the BSS section
 *   2. Initialize the CLUU runtime (heap, fd table)
 *   3. Read argc/argv from the ProcessInfo page
 *   4. Call main(argc, argv, envp)
 *   5. Call _exit with the return value
 *
 * Memory layout (from user.ld):
 *   0x00400000 - Code (.text)
 *   0x00500000+ - Data (.rodata, .data, .bss)
 *   0x00800000 - Dynamic heap start
 *   0x40000000 - Heap maximum
 *   0x7F000000 - Stack bottom (grows down)
 *   0x80000000 - Stack top
 *
 * ProcessInfo page layout:
 *   0x7FE00000 - Page base
 *   0x7FE00100 - ProcessInfo struct (216 bytes)
 *     +200: params[6] = argc (u64)
 *     +208: params[7] = argv data byte offset within page (u64)
 *   argv data: null-terminated strings packed contiguously after ProcessInfo
 */

.section .text._start
.global _start
.type _start, @function

/* ProcessInfo addresses */
.set PROCESS_INFO_ADDR, 0x7FE00100
.set PAGE_BASE,         0x7FE00000
.set PARAMS_ARGC,       PROCESS_INFO_ADDR + 200  /* params[6] */
.set PARAMS_ARGV_OFF,   PROCESS_INFO_ADDR + 208  /* params[7] */

_start:
    /* Align stack to 16 bytes before any call (SysV ABI). */
    sub     $8, %rsp
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
     *   - Current working directory
     */
    call    __cluu_init

    /*
     * Read argc/argv from ProcessInfo page.
     *
     * ProcessInfo.params[6] = argc
     * ProcessInfo.params[7] = byte offset in page where argv strings start
     *
     * The argv strings are null-terminated and packed contiguously:
     *   "path\0arg0\0arg1\0..."
     *
     * We build an argv[] pointer array on the stack.
     */
    movabs  $PARAMS_ARGC, %rax
    movq    (%rax), %r12                 /* r12 = argc */
    test    %r12, %r12
    jz      .Lno_argv                    /* if argc == 0, skip argv parsing */

    /* Get pointer to argv string data */
    movabs  $PARAMS_ARGV_OFF, %rax
    movq    (%rax), %r13                 /* r13 = argv data offset in page */
    test    %r13, %r13
    jz      .Lno_argv
    movabs  $PAGE_BASE, %rax
    addq    %r13, %rax                   /* rax = pointer to argv string data */
    movq    %rax, %r13                   /* r13 = argv data pointer */

    /*
     * Build argv[] array on the stack.
     * We need (argc + 1) pointers (the +1 is for the NULL terminator).
     * Each pointer is 8 bytes.
     */
    leaq    1(%r12), %rcx               /* rcx = argc + 1 */
    shlq    $3, %rcx                    /* rcx = (argc + 1) * 8 */
    /* Align to 16 bytes */
    addq    $15, %rcx
    andq    $-16, %rcx
    subq    %rcx, %rsp                  /* allocate argv[] on stack */
    movq    %rsp, %r14                  /* r14 = argv[] base */

    /* Scan argv strings and fill pointer array */
    movq    %r13, %rsi                  /* rsi = current string pointer */
    xorq    %rcx, %rcx                  /* rcx = index */

.Largv_loop:
    cmpq    %r12, %rcx                  /* if index >= argc, done */
    jge     .Largv_done
    movq    %rsi, (%r14,%rcx,8)         /* argv[index] = current string */
    incq    %rcx

    /* Scan to next '\0' */
.Lscan_nul:
    cmpb    $0, (%rsi)
    je      .Lfound_nul
    incq    %rsi
    jmp     .Lscan_nul

.Lfound_nul:
    incq    %rsi                        /* skip past the '\0' */
    jmp     .Largv_loop

.Largv_done:
    /* argv[argc] = NULL */
    movq    $0, (%r14,%r12,8)

    /* Call main(argc, argv, envp) */
    movq    %r12, %rdi                  /* argc */
    movq    %r14, %rsi                  /* argv */
    xorq    %rdx, %rdx                  /* envp = NULL */
    call    main
    jmp     .Lexit

.Lno_argv:
    /* No arguments — call main(0, NULL, NULL) */
    xor     %edi, %edi                  /* argc = 0 */
    xor     %esi, %esi                  /* argv = NULL */
    xor     %edx, %edx                  /* envp = NULL */
    call    main

.Lexit:
    /*
     * Exit with main's return value.
     * main() returns int in %eax, pass it to _exit.
     */
    mov     %eax, %edi                  /* status = return value */
    call    _exit

    /* Should never reach here, but loop just in case */
.Lhang:
    hlt
    jmp     .Lhang

.size _start, . - _start

/*
 * __cluu_init is provided by libcluu_syscalls.a
 * It initializes the heap allocator and fd table.
 * Linking will fail if libcluu_syscalls.a is not linked.
 */
