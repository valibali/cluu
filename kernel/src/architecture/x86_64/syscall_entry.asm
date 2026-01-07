; ═══════════════════════════════════════════════════════════════════════════
; CLUU Microkernel - Syscall Entry Point (x86_64)
; ═══════════════════════════════════════════════════════════════════════════
;
; This file implements the low-level syscall entry/exit for the SYSCALL
; instruction on x86_64. It saves a full CPU context so the scheduler can
; preempt and restore threads safely.
;
; # Syscall Convention (matches Linux for compatibility)
;
; Entry:
;   RAX = syscall number
;   RDI = arg1
;   RSI = arg2
;   RDX = arg3
;   R10 = arg4 (R10 instead of RCX because SYSCALL clobbers RCX)
;   R8  = arg5
;   R9  = arg6
;
; Exit:
;   RAX = return value (positive) or -errno (negative)
;
; # SYSCALL Instruction Behavior
;
; The SYSCALL instruction does:
;   RCX ← RIP (return address)
;   R11 ← RFLAGS
;   RIP ← IA32_LSTAR
;   CS  ← IA32_STAR[47:32]
;   SS  ← IA32_STAR[47:32] + 8
;   RFLAGS ← RFLAGS & ~IA32_FMASK
;
; # Return Path
;
; We return to userspace with IRETQ so we can restore the full register state.
;
[BITS 64]
section .text

global syscall_entry
extern syscall_dispatch
extern schedule_and_switch

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

syscall_entry:
    ; At this point:
    ; - RCX = user RIP (return address)
    ; - R11 = user RFLAGS
    ; - CS/SS switched to kernel segments
    ; - Still using user stack!
    ; - RAX = syscall number

    ; ───────────────────────────────────────────────────────────────────────
    ; Switch to kernel stack
    ; ───────────────────────────────────────────────────────────────────────

    swapgs                          ; Switch to kernel GS base

    mov [gs:0x00], rsp              ; Save user RSP to per-CPU area
    mov rsp, [gs:0x08]              ; Load kernel RSP from per-CPU area

    ; Preserve syscall number before clobbering RAX
    mov r13, rax

    ; ───────────────────────────────────────────────────────────────────────
    ; Save full context on kernel stack
    ; ───────────────────────────────────────────────────────────────────────

    sub rsp, CONTEXT_SIZE

    ; General-purpose registers
    mov [rsp + CONTEXT_RAX], r13
    mov [rsp + CONTEXT_RBX], rbx
    mov qword [rsp + CONTEXT_RCX], 0        ; User RCX is clobbered by SYSCALL
    mov [rsp + CONTEXT_RDX], rdx
    mov [rsp + CONTEXT_RSI], rsi
    mov [rsp + CONTEXT_RDI], rdi
    mov [rsp + CONTEXT_R8], r8
    mov [rsp + CONTEXT_R9], r9
    mov [rsp + CONTEXT_R10], r10
    mov qword [rsp + CONTEXT_R11], 0        ; User R11 is clobbered by SYSCALL
    mov [rsp + CONTEXT_R12], r12
    mov [rsp + CONTEXT_R13], r13
    mov [rsp + CONTEXT_R14], r14
    mov [rsp + CONTEXT_R15], r15
    mov [rsp + CONTEXT_RBP], rbp

    ; Stack and instruction pointers
    mov rax, [gs:0x00]
    mov [rsp + CONTEXT_RSP], rax
    mov [rsp + CONTEXT_RIP], rcx
    mov [rsp + CONTEXT_RFLAGS], r11

    ; Segment selectors
    mov qword [rsp + CONTEXT_CS], 0x33
    mov qword [rsp + CONTEXT_SS], 0x2b

    ; CR3
    mov rax, cr3
    mov [rsp + CONTEXT_CR3], rax

    ; Keep a stable pointer to the context
    mov r15, rsp

    ; ───────────────────────────────────────────────────────────────────────
    ; Prepare arguments for syscall_dispatch
    ; ───────────────────────────────────────────────────────────────────────

    ; Map syscall register arguments to System V ABI:
    ; RDI = syscall number (from RAX)
    ; RSI = arg1 (from user RDI)
    ; RDX = arg2 (from user RSI)
    ; RCX = arg3 (from user RDX)
    ; R8  = arg4 (from user R10)
    ; R9  = arg5 (from user R8)
    ; [rsp] = arg6 (from user R9)

    mov r11, r9                     ; Save arg6 for stack
    mov r9, r8                      ; arg5 (user R8)
    mov r8, r10                     ; arg4 (user R10)
    mov rcx, rdx                    ; arg3 (user RDX)
    mov rdx, rsi                    ; arg2 (user RSI)
    mov rsi, rdi                    ; arg1 (user RDI)
    mov rdi, r13                    ; syscall number

    sub rsp, 8                      ; Align stack for call
    mov [rsp], r11                  ; arg6 on stack

    ; Re-enable interrupts now that we're safely on kernel stack
    sti

    call syscall_dispatch

    add rsp, 8                      ; Drop arg6 and restore alignment

    ; Save syscall return value into context
    mov [r15 + CONTEXT_RAX], rax

    ; ───────────────────────────────────────────────────────────────────────
    ; Context Switch Check
    ; ───────────────────────────────────────────────────────────────────────

    mov rdi, r15
    sub rsp, 8                      ; Align stack for call
    call schedule_and_switch
    add rsp, 8

    ; RAX = pointer to next thread's Context (or NULL if no switch)
    mov r10, rax
    test r10, r10
    jnz .have_next_context
    mov r10, r15                    ; No switch: use current context

.have_next_context:
    ; ───────────────────────────────────────────────────────────────────────
    ; Restore next context and return to userspace
    ; ───────────────────────────────────────────────────────────────────────

    ; Switch address space first
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build user interrupt frame on kernel stack
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]

    push rax                        ; SS
    push rbx                        ; RSP
    push rcx                        ; RFLAGS
    push rdx                        ; CS
    push rsi                        ; RIP

    ; Restore general-purpose registers
    mov rax, [r10 + CONTEXT_RAX]
    mov rbx, [r10 + CONTEXT_RBX]
    mov rcx, [r10 + CONTEXT_RCX]
    mov rdx, [r10 + CONTEXT_RDX]
    mov rsi, [r10 + CONTEXT_RSI]
    mov rdi, [r10 + CONTEXT_RDI]
    mov r8, [r10 + CONTEXT_R8]
    mov r9, [r10 + CONTEXT_R9]
    mov r11, [r10 + CONTEXT_R11]
    mov r12, [r10 + CONTEXT_R12]
    mov r13, [r10 + CONTEXT_R13]
    mov r14, [r10 + CONTEXT_R14]
    mov r15, [r10 + CONTEXT_R15]
    mov rbp, [r10 + CONTEXT_RBP]
    mov r10, [r10 + CONTEXT_R10]

    swapgs
    cli
    iretq

; ═══════════════════════════════════════════════════════════════════════════
; Notes
; ═══════════════════════════════════════════════════════════════════════════
;
; # Per-CPU Data Layout (GS-relative)
;
; Offset  Size  Description
; ------  ----  -----------
; 0x00    8     User RSP (saved during syscall)
; 0x08    8     Kernel RSP (loaded during syscall)
; 0x10    8     Current thread pointer (future)
; 0x18    8     CPU ID (future)
;
; # Stack Alignment
;
; The System V AMD64 ABI requires RSP to be 16-byte aligned before CALL.
; We align before each Rust call by subtracting 8 bytes.
;
