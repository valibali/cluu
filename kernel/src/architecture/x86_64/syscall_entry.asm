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
extern syscall_dispatch
extern schedule_and_switch

; Per-CPU data offsets (must match PerCpuData struct in syscall.rs)
%define PERCPU_USER_RSP        0x00
%define PERCPU_KERNEL_RSP      0x08
%define PERCPU_NEED_RESCHED    0x20   ; u64 flag set by request_resched()

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
%define CONTEXT_SIZE    0xA8

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

    mov [gs:PERCPU_USER_RSP], rsp       ; Save user RSP
    mov rsp, [gs:PERCPU_KERNEL_RSP]     ; Load kernel RSP

    ; ─────────────────────────────────────────────────────────────────────────
    ; Fast path: Save only what SYSCALL clobbers + syscall number
    ; ─────────────────────────────────────────────────────────────────────────
    ; SYSCALL clobbers: RCX (RIP), R11 (RFLAGS)
    ; We also need to save RAX (syscall number) to restore on return

    push rcx                            ; User RIP
    push r11                            ; User RFLAGS
    push rax                            ; Syscall number (for debug, not needed for return)

    ; Save callee-saved registers (SysV ABI: RBX, RBP, R12-R15)
    ; We need these because syscall_dispatch is a C function
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15

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
    ; Call syscall handler
    ; ─────────────────────────────────────────────────────────────────────────
    sti                                 ; Enable interrupts during syscall
    call syscall_dispatch
    cli                                 ; Disable interrupts for return path

    add rsp, 8                          ; Pop arg6 from stack

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

    ; Restore callee-saved registers
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx

    ; Skip saved syscall number (we don't need it)
    add rsp, 8

    ; Restore RFLAGS and RIP for SYSRET
    pop r11                             ; User RFLAGS -> R11
    pop rcx                             ; User RIP -> RCX

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

    ; Restore callee-saved to get their values for context save
    pop r14                             ; Actually r15 from stack
    mov rax, r14                        ; Save it
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx

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

    ; Get saved RFLAGS
    mov r11, [rsp + CONTEXT_SIZE + 8]   ; R11 (RFLAGS) was pushed second
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

    ; Switch address space if needed
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build IRETQ frame on stack
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]

    push rax                            ; SS
    push rbx                            ; RSP
    push rcx                            ; RFLAGS
    push rdx                            ; CS
    push rsi                            ; RIP

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

    ; Return to userspace
    swapgs
    iretq
