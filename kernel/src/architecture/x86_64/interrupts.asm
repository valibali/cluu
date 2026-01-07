; ═══════════════════════════════════════════════════════════════════════════
; CLUU Microkernel - Interrupt Entry Points (x86_64)
; ═══════════════════════════════════════════════════════════════════════════
;
; Timer IRQ entry point that saves full CPU context and delegates scheduling
; to the Rust scheduler. The Rust side decides whether to switch threads.
;
[BITS 64]
section .text

global timer_interrupt_entry
extern timer_interrupt_dispatch
extern timer_interrupt_ack

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

; ─────────────────────────────────────────────────────────────────────────
; Timer interrupt entry (IRQ 0)
; ─────────────────────────────────────────────────────────────────────────

timer_interrupt_entry:
    ; Reserve space for Context
    sub rsp, CONTEXT_SIZE

    ; Save general-purpose registers
    mov [rsp + CONTEXT_RAX], rax
    mov [rsp + CONTEXT_RBX], rbx
    mov [rsp + CONTEXT_RCX], rcx
    mov [rsp + CONTEXT_RDX], rdx
    mov [rsp + CONTEXT_RSI], rsi
    mov [rsp + CONTEXT_RDI], rdi
    mov [rsp + CONTEXT_R8], r8
    mov [rsp + CONTEXT_R9], r9
    mov [rsp + CONTEXT_R10], r10
    mov [rsp + CONTEXT_R11], r11
    mov [rsp + CONTEXT_R12], r12
    mov [rsp + CONTEXT_R13], r13
    mov [rsp + CONTEXT_R14], r14
    mov [rsp + CONTEXT_R15], r15
    mov [rsp + CONTEXT_RBP], rbp

    ; Extract interrupt frame (below context)
    mov rax, [rsp + CONTEXT_SIZE + 0x00]   ; RIP
    mov [rsp + CONTEXT_RIP], rax
    mov rax, [rsp + CONTEXT_SIZE + 0x08]   ; CS
    mov [rsp + CONTEXT_CS], rax
    mov rax, [rsp + CONTEXT_SIZE + 0x10]   ; RFLAGS
    mov [rsp + CONTEXT_RFLAGS], rax

    ; Check CPL (user mode?)
    mov rax, [rsp + CONTEXT_SIZE + 0x08]
    test al, 0x3
    jz .kernel_path

    ; User mode interrupt: SS/RSP are on the stack
    mov rax, [rsp + CONTEXT_SIZE + 0x18]   ; RSP
    mov [rsp + CONTEXT_RSP], rax
    mov rax, [rsp + CONTEXT_SIZE + 0x20]   ; SS
    mov [rsp + CONTEXT_SS], rax

    ; CR3
    mov rax, cr3
    mov [rsp + CONTEXT_CR3], rax

    ; Call scheduler dispatch
    mov rdi, rsp
    sub rsp, 8
    call timer_interrupt_dispatch
    add rsp, 8

    ; RAX = next context (or NULL)
    mov r10, rax
    test r10, r10
    jnz .have_next_context
    mov r10, rsp

.have_next_context:
    ; Switch address space
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build user interrupt frame on kernel stack
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]

    push rax
    push rbx
    push rcx
    push rdx
    push rsi

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

    iretq

.kernel_path:
    ; Kernel mode: acknowledge and return without scheduling
    sub rsp, 8
    call timer_interrupt_ack
    add rsp, 8

    ; Restore registers
    mov rax, [rsp + CONTEXT_RAX]
    mov rbx, [rsp + CONTEXT_RBX]
    mov rcx, [rsp + CONTEXT_RCX]
    mov rdx, [rsp + CONTEXT_RDX]
    mov rsi, [rsp + CONTEXT_RSI]
    mov rdi, [rsp + CONTEXT_RDI]
    mov r8, [rsp + CONTEXT_R8]
    mov r9, [rsp + CONTEXT_R9]
    mov r10, [rsp + CONTEXT_R10]
    mov r11, [rsp + CONTEXT_R11]
    mov r12, [rsp + CONTEXT_R12]
    mov r13, [rsp + CONTEXT_R13]
    mov r14, [rsp + CONTEXT_R14]
    mov r15, [rsp + CONTEXT_R15]
    mov rbp, [rsp + CONTEXT_RBP]

    add rsp, CONTEXT_SIZE
    iretq

