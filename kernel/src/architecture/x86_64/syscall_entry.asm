; ═══════════════════════════════════════════════════════════════════════════
; CLUU Microkernel - Syscall Entry Point (x86_64)
; ═══════════════════════════════════════════════════════════════════════════
;
; Fast/Slow Path Design:
;   - Fast path: Most syscalls (no context switch) - minimal save, SYSRET
;   - Slow path: Context switch needed - full save, IRETQ
;
; SYSRET is ~40 cycles faster than IRETQ for the common case.
;
[BITS 64]
section .text

global syscall_entry
global enter_userspace_asm
global sysv_abi_preservation_test
extern syscall_dispatch
extern syscall_ipc_send
extern syscall_ipc_recv
extern syscall_ipc_call
extern syscall_ipc_reply
extern schedule_and_switch
extern abi_check_callee

; Per-CPU data offsets (must match PerCpuData struct in syscall.rs)
%define PERCPU_USER_RSP        0x00
%define PERCPU_KERNEL_RSP      0x08
%define PERCPU_NEED_RESCHED    0x20   ; u64 flag set by request_resched()
%define PERCPU_LAST_SYSNO      0x28
%define PERCPU_LAST_RIP        0x30
%define PERCPU_LAST_RSP        0x38
%define PERCPU_LAST_RBX        0x40
%define PERCPU_LAST_ARG1       0x48
%define PERCPU_LAST_ARG2       0x50
%define PERCPU_LAST_ARG3       0x58
%define PERCPU_LAST_ARG4       0x60
%define PERCPU_LAST_ARG5       0x68
%define PERCPU_LAST_ARG6       0x70
%define PERCPU_LAST_RBX_RET    0x78
%define PERCPU_FPU_SCRATCH     0x80

; Full context structure (for slow path)
%define CONTEXT_RAX     0x00
%define CONTEXT_RBX     0x08
%define CONTEXT_RCX     0x10
%define CONTEXT_RDX     0x18
%define CONTEXT_RSI     0x20
%define CONTEXT_RDI     0x28
%define CONTEXT_R8      0x30
%define CONTEXT_R9      0x38
%define CONTEXT_R10     0x40
%define CONTEXT_R11     0x48
%define CONTEXT_R12     0x50
%define CONTEXT_R13     0x58
%define CONTEXT_R14     0x60
%define CONTEXT_R15     0x68
%define CONTEXT_RBP     0x70
%define CONTEXT_RSP     0x78
%define CONTEXT_RIP     0x80
%define CONTEXT_RFLAGS  0x88
%define CONTEXT_CS      0x90
%define CONTEXT_SS      0x98
%define CONTEXT_CR3     0xA0
%define CONTEXT_FS_BASE 0xA8
%define CONTEXT_SIZE    0xB8

; ═══════════════════════════════════════════════════════════════════════════
; Syscall Entry Point
; ═══════════════════════════════════════════════════════════════════════════
;
; Entry state (set by SYSCALL instruction):
;   RCX = user RIP (return address)
;   R11 = user RFLAGS
;   RAX = syscall number
;   RDI, RSI, RDX, R10, R8, R9 = syscall arguments
;   RSP = still user stack
;   CS/SS = kernel segments (from STAR MSR)
;
syscall_entry:
    ; ─────────────────────────────────────────────────────────────────────────
    ; Switch to kernel context
    ; ─────────────────────────────────────────────────────────────────────────
    swapgs                              ; Switch GS to kernel PerCpuData
    clac                                ; Enforce SMAP (clear AC flag)

    ; User RBX is required by the fast return path (SYSRET restores it)
    mov [gs:PERCPU_LAST_RBX], rbx
%ifdef DEBUG
    ; Debug trace: capture last syscall context (user values)
    mov [gs:PERCPU_LAST_SYSNO], rax     ; Syscall number
    mov [gs:PERCPU_LAST_RIP], rcx       ; User RIP (from SYSCALL)
    mov [gs:PERCPU_LAST_RSP], rsp       ; User RSP (still in RSP)
    mov [gs:PERCPU_LAST_ARG1], rdi      ; Arg1 (RDI)
    mov [gs:PERCPU_LAST_ARG2], rsi      ; Arg2 (RSI)
    mov [gs:PERCPU_LAST_ARG3], rdx      ; Arg3 (RDX)
    mov [gs:PERCPU_LAST_ARG4], r10      ; Arg4 (R10)
    mov [gs:PERCPU_LAST_ARG5], r8       ; Arg5 (R8)
    mov [gs:PERCPU_LAST_ARG6], r9       ; Arg6 (R9)
%endif

    mov [gs:PERCPU_USER_RSP], rsp       ; Save user RSP
    mov rsp, [gs:PERCPU_KERNEL_RSP]     ; Load kernel RSP (already aligned)

    ; Save user FPU/SSE state to per-CPU scratch buffer.
    ; Must happen before any Rust code runs (compiler may use SSE).
    fxsave [gs:PERCPU_FPU_SCRATCH]

    ; ─────────────────────────────────────────────────────────────────────────
    ; Fast path: Save only what SYSCALL clobbers + syscall number
    ; ─────────────────────────────────────────────────────────────────────────
    ; SYSCALL clobbers: RCX (RIP), R11 (RFLAGS)
    ; We also need to save RAX (syscall number) to restore on return

    push rcx                            ; User RIP
    push r11                            ; User RFLAGS
    push rax                            ; Syscall number (for debug, not needed for return)

    ; Save R15 (clobbered below by mov r15, rax; other callee-saved regs
    ; are preserved by syscall_dispatch per SysV ABI)
    push r15

    ; 16-byte stack alignment: 4 pushes above (rcx, r11, rax, r15) = 32 bytes
    ; (already 16-aligned). Adding arg6 below would make 40 bytes (misaligned).
    ; Pad here so push r9 (arg6) gives 48 bytes total = 16-aligned for call.
    sub rsp, 8

    ; ─────────────────────────────────────────────────────────────────────────
    ; Marshal arguments for syscall_dispatch (SysV ABI)
    ; ─────────────────────────────────────────────────────────────────────────
    ; Syscall args:  RAX=number, RDI, RSI, RDX, R10, R8, R9
    ; SysV ABI:      RDI, RSI, RDX, RCX, R8, R9, [stack]
    ;
    ; syscall_dispatch(number, arg1, arg2, arg3, arg4, arg5, arg6)
    ;                  RDI     RSI   RDX   RCX   R8    R9    [rsp]

    mov r15, rax                        ; Save syscall number

    ; arg6 goes on stack (R9 from userspace)
    push r9

    ; Shift args: syscall R10->RCX (arg4), keep R8, R9 from user
    mov rax, r9                         ; Save user R9 (arg6)
    mov r9, r8                          ; arg5 = R8
    mov r8, r10                         ; arg4 = R10
    mov rcx, rdx                        ; arg3 = RDX
    mov rdx, rsi                        ; arg2 = RSI
    mov rsi, rdi                        ; arg1 = RDI
    mov rdi, r15                        ; number = saved RAX

    ; arg6 already on stack from push r9 above, but we pushed the wrong value
    ; Fix: put original R9 (user arg6) on stack
    mov [rsp], rax

    ; ─────────────────────────────────────────────────────────────────────────
    ; IPC fast-path: syscall numbers 0-3 branch directly to dedicated handlers
    ; Skips SyscallNumber::from_usize + dispatch_syscall match overhead
    ; ─────────────────────────────────────────────────────────────────────────
    cmp r15, 3                          ; r15 = saved syscall number
    ja .generic_dispatch                ; >3 → generic path

    lea rax, [rel .ipc_jump_table]
    jmp [rax + r15 * 8]

.ipc_jump_table:
    dq .ipc_send                        ; 0 = Send
    dq .ipc_recv                        ; 1 = Recv
    dq .ipc_call                        ; 2 = Call
    dq .ipc_reply                       ; 3 = Reply

.ipc_send:
    call syscall_ipc_send
    jmp .after_dispatch
.ipc_recv:
    call syscall_ipc_recv
    jmp .after_dispatch
.ipc_call:
    call syscall_ipc_call
    jmp .after_dispatch
.ipc_reply:
    call syscall_ipc_reply
    jmp .after_dispatch

.generic_dispatch:
    call syscall_dispatch
.after_dispatch:

    add rsp, 16                         ; Pop arg6 + alignment pad from stack

    ; RAX now contains return value

    ; ─────────────────────────────────────────────────────────────────────────
    ; Check if context switch is requested
    ; ─────────────────────────────────────────────────────────────────────────
    cmp qword [gs:PERCPU_NEED_RESCHED], 0
    jne .slow_path

    ; ─────────────────────────────────────────────────────────────────────────
    ; FAST PATH: Return via SYSRET (no context switch)
    ; ─────────────────────────────────────────────────────────────────────────
    ; RAX = return value (already set)

    ; Restore R15 (only callee-saved we pushed); R12-R14, RBP preserved by ABI
    pop r15
    mov rbx, [gs:PERCPU_LAST_RBX]
%ifdef DEBUG
    mov [gs:PERCPU_LAST_RBX_RET], rbx
%endif

    ; Skip saved syscall number (we don't need it)
    add rsp, 8

    ; Restore RFLAGS and RIP for SYSRET
    pop r11                             ; User RFLAGS -> R11
    pop rcx                             ; User RIP -> RCX

    ; ─────────────────────────────────────────────────────────────────────────
    ; SECURITY: Sanitize RFLAGS before returning to userspace
    ; Clear: TF (bit 8, single-step), NT (bit 14, nested task),
    ;        IOPL (bits 12-13), AC (bit 18, alignment check),
    ;        RF (bit 16, resume flag)
    ; Ensure: IF (bit 9) and reserved bit 1 are set
    ; ─────────────────────────────────────────────────────────────────────────
    and r11, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  r11, (1 << 9) | (1 << 1)        ; IF + reserved bit 1

    ; ─────────────────────────────────────────────────────────────────────────
    ; SECURITY: Validate RCX is canonical user address before SYSRET
    ; Intel SYSRET bug: if RCX is non-canonical, #GP fires at CPL 0 after
    ; partial instruction execution, potentially allowing privilege escalation.
    ; User canonical addresses: bits 63:47 must all be 0 (0 to 0x7FFFFFFFFFFF)
    ; ─────────────────────────────────────────────────────────────────────────
    mov r10, rcx
    shr r10, 47
    test r10, r10
    jnz .sysret_unsafe_fallback          ; Non-zero = non-canonical or kernel addr

    ; Restore user stack
    mov rsp, [gs:PERCPU_USER_RSP]

    ; Initialize userspace segment registers (DS, ES)
    ; SYSRET sets CS/SS automatically, but DS/ES need manual init
    ; NOTE: Do NOT set FS here — it would zero the FS base (MSR 0xC0000100)
    ; used for TLS. No context switch occurred, so FS base is still correct.
    mov r10w, 0x2b
    mov ds, r10w
    mov es, r10w

    ; Restore user FPU/SSE state from per-CPU scratch buffer before returning
    fxrstor [gs:PERCPU_FPU_SCRATCH]

    ; Return to userspace via fast SYSRET
    swapgs
    o64 sysret

    ; ─────────────────────────────────────────────────────────────────────────
    ; SYSRET fallback: use safe IRETQ for non-canonical addresses
    ; ─────────────────────────────────────────────────────────────────────────
.sysret_unsafe_fallback:
    ; Build IRETQ frame on kernel stack (still have kernel RSP)
    push 0x2b                           ; User SS (0x28 | RPL 3)
    push qword [gs:PERCPU_USER_RSP]     ; User RSP
    push r11                            ; User RFLAGS
    push 0x33                           ; User CS (0x30 | RPL 3)
    push rcx                            ; User RIP (possibly non-canonical)

    ; Initialize userspace segment registers (DS, ES)
    ; NOTE: Do NOT set FS — would zero FS base used for TLS.
    mov r10w, 0x2b
    mov ds, r10w
    mov es, r10w

    ; Restore user FPU/SSE state from per-CPU scratch buffer
    fxrstor [gs:PERCPU_FPU_SCRATCH]

    swapgs
    iretq                               ; IRETQ safely handles non-canonical RIP

; ═══════════════════════════════════════════════════════════════════════════
; SLOW PATH: Full context switch
; ═══════════════════════════════════════════════════════════════════════════
.slow_path:
    ; Clear the resched flag
    mov qword [gs:PERCPU_NEED_RESCHED], 0

    ; Save return value
    mov r15, rax

    ; Pop user R15 (only callee-saved pushed); R12-R14, RBP still hold user values
    pop rax                             ; User R15 → rax
    ; RBX from PerCpuData capture
    mov rbx, [gs:PERCPU_LAST_RBX]
%ifdef DEBUG
    mov [gs:PERCPU_LAST_RBX_RET], rbx
%endif

    ; Now stack has: syscall_num, r11 (rflags), rcx (rip)
    ; And we have the callee-saved values in registers

    ; We need to build a full Context structure for schedule_and_switch
    ; Allocate context on stack
    sub rsp, CONTEXT_SIZE

    ; Save return value as RAX in context
    mov [rsp + CONTEXT_RAX], r15
    mov [rsp + CONTEXT_RBX], rbx

    ; Stack layout after pops: [rsp+0]=syscall_num, [rsp+8]=r11, [rsp+16]=rcx
    ; After sub rsp, CONTEXT_SIZE: these are at CONTEXT_SIZE + 0/8/16

    ; Get saved RIP from where we pushed it
    mov rcx, [rsp + CONTEXT_SIZE + 16]  ; RCX (user RIP) was pushed first
    mov [rsp + CONTEXT_RIP], rcx
    mov qword [rsp + CONTEXT_RCX], 0    ; RCX is clobbered by syscall

    ; Get saved RFLAGS and sanitize before storing
    mov r11, [rsp + CONTEXT_SIZE + 8]   ; R11 (RFLAGS) was pushed second
    and r11, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  r11, (1 << 9) | (1 << 1)        ; IF + reserved bit 1
    mov [rsp + CONTEXT_RFLAGS], r11
    mov qword [rsp + CONTEXT_R11], 0    ; R11 is clobbered by syscall

    ; RDX, RSI, RDI were clobbered by our arg marshaling - we can't recover them
    ; But that's OK - syscall clobbers them anyway per ABI
    mov qword [rsp + CONTEXT_RDX], 0
    mov qword [rsp + CONTEXT_RSI], 0
    mov qword [rsp + CONTEXT_RDI], 0
    mov qword [rsp + CONTEXT_R8], 0
    mov qword [rsp + CONTEXT_R9], 0
    mov qword [rsp + CONTEXT_R10], 0

    mov [rsp + CONTEXT_R12], r12
    mov [rsp + CONTEXT_R13], r13
    mov [rsp + CONTEXT_R14], r14
    mov [rsp + CONTEXT_R15], rax        ; r15 was saved in rax
    mov [rsp + CONTEXT_RBP], rbp

    ; User RSP
    mov rax, [gs:PERCPU_USER_RSP]
    mov [rsp + CONTEXT_RSP], rax

    ; Segment selectors for userspace
    mov qword [rsp + CONTEXT_CS], 0x33  ; User CS (0x30 | 3)
    mov qword [rsp + CONTEXT_SS], 0x2b  ; User SS (0x28 | 3)

    ; CR3
    mov rax, cr3
    mov [rsp + CONTEXT_CR3], rax

    ; FS base (TLS) — save via rdmsr(MSR_FS_BASE)
    mov ecx, 0xC0000100                 ; MSR_FS_BASE
    rdmsr                               ; EDX:EAX = FS base
    shl rdx, 32
    or  rax, rdx
    mov [rsp + CONTEXT_FS_BASE], rax

    ; ─────────────────────────────────────────────────────────────────────────
    ; Call scheduler
    ; ─────────────────────────────────────────────────────────────────────────
    mov rdi, rsp                        ; Current context pointer
    call schedule_and_switch

    ; RAX = pointer to next thread's context (or null if same thread)
    mov r10, rax
    test r10, r10
    jnz .have_next_context

    ; No switch - use current context
    mov r10, rsp

.have_next_context:
    ; ─────────────────────────────────────────────────────────────────────────
    ; Restore context and return via IRETQ
    ; ─────────────────────────────────────────────────────────────────────────

    ; Validate RIP before restoring (sanity check for memory corruption)
    mov rsi, [r10 + CONTEXT_RIP]
    ; RIP should be in userspace range (0x400000 - 0x7FFFFFFFFFFF)
    ; Very low addresses (< 0x1000) are almost certainly invalid
    cmp rsi, 0x1000
    jb .invalid_rip
    ; Check if canonical (bits 63:47 must be 0 for userspace)
    mov rax, rsi
    shr rax, 47
    test rax, rax
    jnz .invalid_rip
    jmp .rip_valid

.invalid_rip:
    ; RIP is corrupted - halt to prevent page fault
    ; The validation in Rust code will log the error before we get here
    ; This is a safety check in case we somehow get a corrupted context
    cli
.halt_loop:
    hlt
    jmp .halt_loop

.rip_valid:
    ; Switch address space if needed
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build IRETQ frame on stack (sanitize RFLAGS from context)
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)        ; IF + reserved bit 1
    mov rdx, [r10 + CONTEXT_CS]

    push rax                            ; SS
    push rbx                            ; RSP
    push rcx                            ; RFLAGS (sanitized)
    push rdx                            ; CS
    push rsi                            ; RIP

    ; Set userspace segment registers (DS, ES)
    mov ax, 0x2b
    mov ds, ax
    mov es, ax

    ; Restore FS base (TLS) via wrmsr — must happen BEFORE GPR restore
    ; (mov fs, ax would zero the base, so we skip it and just set MSR directly)
    mov rax, [r10 + CONTEXT_FS_BASE]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xC0000100                 ; MSR_FS_BASE
    wrmsr

    ; Restore all general-purpose registers
    mov rax, [r10 + CONTEXT_RAX]
    mov rbx, [r10 + CONTEXT_RBX]
    mov rcx, [r10 + CONTEXT_RCX]
    mov rdx, [r10 + CONTEXT_RDX]
    mov rsi, [r10 + CONTEXT_RSI]
    mov rdi, [r10 + CONTEXT_RDI]
    mov r8,  [r10 + CONTEXT_R8]
    mov r9,  [r10 + CONTEXT_R9]
    mov r11, [r10 + CONTEXT_R11]
    mov r12, [r10 + CONTEXT_R12]
    mov r13, [r10 + CONTEXT_R13]
    mov r14, [r10 + CONTEXT_R14]
    mov r15, [r10 + CONTEXT_R15]
    mov rbp, [r10 + CONTEXT_RBP]
    mov r10, [r10 + CONTEXT_R10]

    ; Restore FPU/SSE state from per-CPU scratch buffer (Rust staged next thread's state)
    fxrstor [gs:PERCPU_FPU_SCRATCH]

    ; Return to userspace
    swapgs
    iretq

; ═══════════════════════════════════════════════════════════════════════════
; enter_userspace_asm — Initial userspace entry (called from Rust)
; ═══════════════════════════════════════════════════════════════════════════
;
; Called once by ThreadManager::start() to jump to the first thread.
; Rust has already set FS base via wrmsr and staged FPU state in scratch.
;
; Arguments:
;   RDI = pointer to Context struct
;
; Does not return.
;
enter_userspace_asm:
    mov r10, rdi

    ; Restore FPU/SSE from per-CPU scratch buffer
    fxrstor [gs:PERCPU_FPU_SCRATCH]

    ; Set userspace segment registers (DS, ES)
    mov ax, 0x2b
    mov ds, ax
    mov es, ax

    ; Build iretq frame from Context
    push qword [r10 + CONTEXT_SS]
    push qword [r10 + CONTEXT_RSP]
    push qword [r10 + CONTEXT_RFLAGS]
    push qword [r10 + CONTEXT_CS]
    push qword [r10 + CONTEXT_RIP]

    swapgs
    iretq

; ═══════════════════════════════════════════════════════════════════════════
; sysv_abi_preservation_test — Boot-time sentinel check
; ═══════════════════════════════════════════════════════════════════════════
;
; The syscall fast path (T1.5) pushes only R15 on entry and trusts that
; syscall_dispatch preserves the remaining SysV callee-saved registers:
; RBX, RBP, R12, R13, R14. If that assumption ever breaks, user state
; corrupts silently on every syscall. This stub exercises that contract:
;
;   - Loads known sentinel values into RBX/RBP/R12/R13/R14.
;   - Calls an extern "C" Rust function (abi_check_callee) designed to
;     spill to callee-saved registers.
;   - Compares each register against its sentinel on return.
;   - Returns 0 on success, or a code identifying the clobbered register.
;
; We save/restore our own caller's callee-saved regs around the test so
; this routine itself stays a well-behaved extern "C" function.
;
sysv_abi_preservation_test:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15

    mov rbx, 0xBBBBBBBB11111111
    mov rbp, 0xBBBBBBBB22222222
    mov r12, 0xBBBBBBBB33333333
    mov r13, 0xBBBBBBBB44444444
    mov r14, 0xBBBBBBBB55555555

    ; 6 pushes above = 48 bytes → misaligned for call; pad to 16.
    sub rsp, 8
    call abi_check_callee
    add rsp, 8

    mov rax, 0

    mov rcx, 0xBBBBBBBB11111111
    cmp rbx, rcx
    jne .fail_rbx
    mov rcx, 0xBBBBBBBB22222222
    cmp rbp, rcx
    jne .fail_rbp
    mov rcx, 0xBBBBBBBB33333333
    cmp r12, rcx
    jne .fail_r12
    mov rcx, 0xBBBBBBBB44444444
    cmp r13, rcx
    jne .fail_r13
    mov rcx, 0xBBBBBBBB55555555
    cmp r14, rcx
    jne .fail_r14

    xor eax, eax
    jmp .done

.fail_rbx:
    mov eax, 1
    jmp .done
.fail_rbp:
    mov eax, 2
    jmp .done
.fail_r12:
    mov eax, 3
    jmp .done
.fail_r13:
    mov eax, 4
    jmp .done
.fail_r14:
    mov eax, 5

.done:
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret
