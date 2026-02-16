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
global gpf_interrupt_entry
global pf_interrupt_entry
extern timer_interrupt_dispatch
extern timer_interrupt_ack
extern timer_interrupt_should_schedule
extern gpf_with_regs
extern pf_with_regs

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

%define GPF_RAX     0x00
%define GPF_RBX     0x08
%define GPF_RCX     0x10
%define GPF_RDX     0x18
%define GPF_RSI     0x20
%define GPF_RDI     0x28
%define GPF_RBP     0x30
%define GPF_R8      0x38
%define GPF_R9      0x40
%define GPF_R10     0x48
%define GPF_R11     0x50
%define GPF_R12     0x58
%define GPF_R13     0x60
%define GPF_R14     0x68
%define GPF_R15     0x70
%define GPF_ERROR   0x78
%define GPF_RIP     0x80
%define GPF_CS      0x88
%define GPF_RFLAGS  0x90
%define GPF_RSP     0x98
%define GPF_SS      0xA0
%define GPF_SIZE    0xA8

%define PF_RAX     0x00
%define PF_RBX     0x08
%define PF_RCX     0x10
%define PF_RDX     0x18
%define PF_RSI     0x20
%define PF_RDI     0x28
%define PF_RBP     0x30
%define PF_R8      0x38
%define PF_R9      0x40
%define PF_R10     0x48
%define PF_R11     0x50
%define PF_R12     0x58
%define PF_R13     0x60
%define PF_R14     0x68
%define PF_R15     0x70
%define PF_ERROR   0x78
%define PF_RIP     0x80
%define PF_CS      0x88
%define PF_RFLAGS  0x90
%define PF_RSP     0x98
%define PF_SS      0xA0
%define PF_SIZE    0xA8

; ─────────────────────────────────────────────────────────────────────────
; General Protection Fault entry (saves full GPR set, supports context switch)
; ─────────────────────────────────────────────────────────────────────────
gpf_interrupt_entry:
    ; Swap GS to kernel if fault came from userspace (test CS without clobbering r10)
    test byte [rsp + 16], 0x3
    jz .gpf_no_swapgs
    swapgs
.gpf_no_swapgs:
    sub rsp, GPF_SIZE

    mov [rsp + GPF_RAX], rax
    mov [rsp + GPF_RBX], rbx
    mov [rsp + GPF_RCX], rcx
    mov [rsp + GPF_RDX], rdx
    mov [rsp + GPF_RSI], rsi
    mov [rsp + GPF_RDI], rdi
    mov [rsp + GPF_RBP], rbp
    mov [rsp + GPF_R8], r8
    mov [rsp + GPF_R9], r9
    mov [rsp + GPF_R10], r10
    mov [rsp + GPF_R11], r11
    mov [rsp + GPF_R12], r12
    mov [rsp + GPF_R13], r13
    mov [rsp + GPF_R14], r14
    mov [rsp + GPF_R15], r15

    ; Copy exception frame fields into struct
    lea r11, [rsp + GPF_SIZE]
    mov r10, [r11]          ; error code
    mov [rsp + GPF_ERROR], r10
    mov r10, [r11 + 8]      ; RIP
    mov [rsp + GPF_RIP], r10
    mov r10, [r11 + 16]     ; CS
    mov [rsp + GPF_CS], r10
    mov r10, [r11 + 24]     ; RFLAGS
    mov [rsp + GPF_RFLAGS], r10

    ; If CPL=3, user RSP/SS are present on stack.
    mov r10, [r11 + 16]
    test r10b, 0x3
    jz .gpf_no_user
    mov r10, [r11 + 32]     ; RSP
    mov [rsp + GPF_RSP], r10
    mov r10, [r11 + 40]     ; SS
    mov [rsp + GPF_SS], r10
    jmp .gpf_have_user
.gpf_no_user:
    mov qword [rsp + GPF_RSP], 0
    mov qword [rsp + GPF_SS], 0
.gpf_have_user:

    mov rdi, rsp
    sub rsp, 8              ; align for call
    call gpf_with_regs
    add rsp, 8

    ; RAX = *const Context (non-null = switch, null = kernel halt — shouldn't reach here)
    test rax, rax
    jz .gpf_halt

    ; Context switch to next thread (same pattern as timer interrupt)
    mov r10, rax

    ; Switch address space
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build iretq frame for target thread
    mov rax, [r10 + CONTEXT_CS]
    test al, 0x3
    jz .gpf_build_kernel_frame

    ; User-mode target
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    ; Sanitize RFLAGS: clear TF/IOPL/NT/RF/AC, ensure IF + reserved bit 1
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]

    ; Build new iretq frame on IST stack (current position is fine)
    push rax                ; SS
    push rbx                ; RSP
    push rcx                ; RFLAGS (sanitized)
    push rdx                ; CS
    push rsi                ; RIP
    swapgs                  ; Kernel→user GS
    jmp .gpf_restore_regs

.gpf_build_kernel_frame:
    mov rsp, [r10 + CONTEXT_RSP]
    push qword [r10 + CONTEXT_SS]
    push qword [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    push rcx
    push qword [r10 + CONTEXT_CS]
    push qword [r10 + CONTEXT_RIP]

.gpf_restore_regs:
    mov ax, 0x2b
    mov ds, ax
    mov es, ax

    ; Restore FS base (TLS) via wrmsr
    mov rax, [r10 + CONTEXT_FS_BASE]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xC0000100                 ; MSR_FS_BASE
    wrmsr

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
    iretq

.gpf_halt:
    cli
    hlt
    jmp .gpf_halt

; ─────────────────────────────────────────────────────────────────────────
; Page Fault entry (saves full GPR set, supports resume + context switch)
; ─────────────────────────────────────────────────────────────────────────
pf_interrupt_entry:
    ; Swap GS to kernel if fault came from userspace (test CS without clobbering r10)
    test byte [rsp + 16], 0x3
    jz .pf_no_swapgs
    swapgs
.pf_no_swapgs:
    sub rsp, PF_SIZE

    mov [rsp + PF_RAX], rax
    mov [rsp + PF_RBX], rbx
    mov [rsp + PF_RCX], rcx
    mov [rsp + PF_RDX], rdx
    mov [rsp + PF_RSI], rsi
    mov [rsp + PF_RDI], rdi
    mov [rsp + PF_RBP], rbp
    mov [rsp + PF_R8], r8
    mov [rsp + PF_R9], r9
    mov [rsp + PF_R10], r10
    mov [rsp + PF_R11], r11
    mov [rsp + PF_R12], r12
    mov [rsp + PF_R13], r13
    mov [rsp + PF_R14], r14
    mov [rsp + PF_R15], r15

    ; Copy exception frame fields into struct
    lea r11, [rsp + PF_SIZE]
    mov r10, [r11]          ; error code
    mov [rsp + PF_ERROR], r10
    mov r10, [r11 + 8]      ; RIP
    mov [rsp + PF_RIP], r10
    mov r10, [r11 + 16]     ; CS
    mov [rsp + PF_CS], r10
    mov r10, [r11 + 24]     ; RFLAGS
    mov [rsp + PF_RFLAGS], r10

    ; If CPL=3, user RSP/SS are present on stack.
    mov r10, [r11 + 16]
    test r10b, 0x3
    jz .pf_no_user
    mov r10, [r11 + 32]     ; RSP
    mov [rsp + PF_RSP], r10
    mov r10, [r11 + 40]     ; SS
    mov [rsp + PF_SS], r10
    jmp .pf_have_user
.pf_no_user:
    mov qword [rsp + PF_RSP], 0
    mov qword [rsp + PF_SS], 0
.pf_have_user:

    mov rdi, rsp
    sub rsp, 8              ; align for call
    call pf_with_regs
    add rsp, 8

    ; RAX = null (resume via iretq) or non-null (context switch)
    test rax, rax
    jz .pf_resume

    ; ── Context switch to next thread ──
    mov r10, rax

    ; Switch address space
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build iretq frame for target thread
    mov rax, [r10 + CONTEXT_CS]
    test al, 0x3
    jz .pf_build_kernel_frame

    ; User-mode target
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    ; Sanitize RFLAGS
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]

    push rax                ; SS
    push rbx                ; RSP
    push rcx                ; RFLAGS (sanitized)
    push rdx                ; CS
    push rsi                ; RIP
    swapgs                  ; Kernel→user GS
    jmp .pf_restore_switch_regs

.pf_build_kernel_frame:
    mov rsp, [r10 + CONTEXT_RSP]
    push qword [r10 + CONTEXT_SS]
    push qword [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    push rcx
    push qword [r10 + CONTEXT_CS]
    push qword [r10 + CONTEXT_RIP]

.pf_restore_switch_regs:
    mov ax, 0x2b
    mov ds, ax
    mov es, ax

    ; Restore FS base (TLS) via wrmsr
    mov rax, [r10 + CONTEXT_FS_BASE]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xC0000100                 ; MSR_FS_BASE
    wrmsr

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
    iretq

.pf_resume:
    ; ── Resume faulting instruction (lazy alloc succeeded) ──
    ; Restore all GPRs from the saved PfDebugFrame
    mov rax, [rsp + PF_RAX]
    mov rbx, [rsp + PF_RBX]
    mov rcx, [rsp + PF_RCX]
    mov rdx, [rsp + PF_RDX]
    mov rsi, [rsp + PF_RSI]
    mov rdi, [rsp + PF_RDI]
    mov rbp, [rsp + PF_RBP]
    mov r8,  [rsp + PF_R8]
    mov r9,  [rsp + PF_R9]
    mov r10, [rsp + PF_R10]
    mov r11, [rsp + PF_R11]
    mov r12, [rsp + PF_R12]
    mov r13, [rsp + PF_R13]
    mov r14, [rsp + PF_R14]
    mov r15, [rsp + PF_R15]

    ; Skip PfDebugFrame + error_code to reach iretq frame (RIP, CS, RFLAGS, RSP, SS)
    add rsp, PF_SIZE + 8

    ; Swap GS back to user mode (resume always returns to userspace)
    swapgs
    iretq

; ─────────────────────────────────────────────────────────────────────────
; Timer interrupt entry (IRQ 0)
; ─────────────────────────────────────────────────────────────────────────

timer_interrupt_entry:
    ; Check if we came from user mode (CS on stack has RPL bits set)
    ; Stack at entry: [rsp+0]=RIP, [rsp+8]=CS, [rsp+16]=RFLAGS, [rsp+24]=RSP, [rsp+32]=SS
    test qword [rsp + 0x08], 0x3
    jz .skip_swapgs_entry
    swapgs                                  ; User->kernel: switch to kernel GS
.skip_swapgs_entry:

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
    jz .kernel_stack
    mov r11d, 1

    ; User mode interrupt: SS/RSP are on the stack
    mov rax, [rsp + CONTEXT_SIZE + 0x18]   ; RSP
    mov [rsp + CONTEXT_RSP], rax
    mov rax, [rsp + CONTEXT_SIZE + 0x20]   ; SS
    mov [rsp + CONTEXT_SS], rax
    jmp .have_stack

.kernel_stack:
    xor r11d, r11d
    ; Kernel mode interrupt: build SS/RSP from current stack
    lea rax, [rsp + CONTEXT_SIZE + 0x18]
    mov [rsp + CONTEXT_RSP], rax
    mov qword [rsp + CONTEXT_SS], 0x10

.have_stack:
    ; CR3
    mov rax, cr3
    mov [rsp + CONTEXT_CR3], rax

    ; FS base (TLS) — save via rdmsr(MSR_FS_BASE)
    mov ecx, 0xC0000100                     ; MSR_FS_BASE
    rdmsr                                   ; EDX:EAX = FS base
    shl rdx, 32
    or  rax, rdx
    mov [rsp + CONTEXT_FS_BASE], rax

    ; If the interrupt happened in kernel mode, only schedule when idle runs.
    test r11d, r11d
    jnz .do_schedule
    push r11
    sub rsp, 8
    call timer_interrupt_should_schedule
    add rsp, 8
    pop r11
    test al, al
    jnz .do_schedule
    push r11
    sub rsp, 8
    call timer_interrupt_ack
    add rsp, 8
    pop r11

    ; Restore general-purpose registers
    mov rax, [rsp + CONTEXT_RAX]
    mov rbx, [rsp + CONTEXT_RBX]
    mov rcx, [rsp + CONTEXT_RCX]
    mov rdx, [rsp + CONTEXT_RDX]
    mov rsi, [rsp + CONTEXT_RSI]
    mov rdi, [rsp + CONTEXT_RDI]
    mov r8, [rsp + CONTEXT_R8]
    mov r9, [rsp + CONTEXT_R9]
    mov r11, [rsp + CONTEXT_R11]
    mov r12, [rsp + CONTEXT_R12]
    mov r13, [rsp + CONTEXT_R13]
    mov r14, [rsp + CONTEXT_R14]
    mov r15, [rsp + CONTEXT_R15]
    mov rbp, [rsp + CONTEXT_RBP]
    mov r10, [rsp + CONTEXT_R10]
    add rsp, CONTEXT_SIZE
    ; Check if returning to user mode (CS on iret frame has RPL bits)
    test qword [rsp + 0x08], 0x3
    jz .no_swapgs_fast_ret
    swapgs                                  ; Kernel->user: restore user GS
.no_swapgs_fast_ret:
    iretq

.do_schedule:
    ; Call scheduler dispatch (preserve r11 flag, keep stack aligned)
    mov rdi, rsp
    push r11
    sub rsp, 8
    call timer_interrupt_dispatch
    add rsp, 8
    pop r11

    ; RAX = next context (or NULL)
    mov r10, rax
    test r10, r10
    jnz .have_next_context
    mov r10, rsp

.have_next_context:
    ; Switch address space
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Build interrupt frame based on target CPL
    mov rax, [r10 + CONTEXT_CS]
    test al, 0x3
    jz .build_kernel_frame

.build_user_frame:
    ; Discard original interrupt frame (user-origin has SS/RSP)
    lea rbx, [rsp + CONTEXT_SIZE]
    test r11d, r11d
    jz .orig_kernel_frame
    add rbx, 40
    jmp .orig_frame_ready
.orig_kernel_frame:
    add rbx, 24
.orig_frame_ready:
    mov rsp, rbx

    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    ; Sanitize RFLAGS: clear TF/IOPL/NT/RF/AC, ensure IF + reserved bit 1
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]

    push rax
    push rbx
    push rcx                                ; RFLAGS (sanitized)
    push rdx
    push rsi
    swapgs                                  ; Kernel->user: restore user GS
    jmp .restore_regs

.build_kernel_frame:
    ; x86_64 IRETQ always pops 5 values (RIP, CS, RFLAGS, RSP, SS)
    ; even for same-privilege returns. Build the full frame.
    ; Use the saved RSP as the post-IRETQ stack pointer.
    mov rsp, [r10 + CONTEXT_RSP]

    push qword [r10 + CONTEXT_SS]
    push qword [r10 + CONTEXT_RSP]
    ; Sanitize RFLAGS for kernel return too (ensure IF is set)
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    push rcx
    push qword [r10 + CONTEXT_CS]
    push qword [r10 + CONTEXT_RIP]

.restore_regs:
    ; Set userspace segment registers (DS, ES)
    mov ax, 0x2b
    mov ds, ax
    mov es, ax

    ; Restore FS base (TLS) via wrmsr
    mov rax, [r10 + CONTEXT_FS_BASE]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xC0000100                     ; MSR_FS_BASE
    wrmsr

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
