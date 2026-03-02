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
global generic_fault_entry_de
global generic_fault_entry_ud
global generic_fault_entry_of
global generic_fault_entry_br
global generic_fault_entry_nm
global generic_fault_entry_mf
global generic_fault_entry_xm
global shared_restore_regs
extern timer_interrupt_dispatch
extern timer_interrupt_ack
extern timer_interrupt_should_schedule
extern gpf_with_regs
extern pf_with_regs
extern generic_fault_with_regs

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

; GenericFaultFrame layout (no error code)
%define GF_RAX     0x00
%define GF_RBX     0x08
%define GF_RCX     0x10
%define GF_RDX     0x18
%define GF_RSI     0x20
%define GF_RDI     0x28
%define GF_RBP     0x30
%define GF_R8      0x38
%define GF_R9      0x40
%define GF_R10     0x48
%define GF_R11     0x50
%define GF_R12     0x58
%define GF_R13     0x60
%define GF_R14     0x68
%define GF_R15     0x70
%define GF_VECTOR  0x78
%define GF_RIP     0x80
%define GF_CS      0x88
%define GF_RFLAGS  0x90
%define GF_RSP     0x98
%define GF_SS      0xA0
%define GF_SIZE    0xA8

; ─────────────────────────────────────────────────────────────────────────
; Generic fault entry macro for exceptions WITHOUT error code
; (Divide Error, Invalid Opcode, Overflow, Bound Range, Device N/A)
; ─────────────────────────────────────────────────────────────────────────
%macro GENERIC_FAULT_ENTRY 2  ; %1 = label, %2 = vector number
%1:
    ; SWAPGS if from userspace
    test byte [rsp + 8], 0x3   ; CS is at [rsp+8] (no error code)
    jz %%no_swapgs
    swapgs
%%no_swapgs:
    sub rsp, GF_SIZE

    mov [rsp + GF_RAX], rax
    mov [rsp + GF_RBX], rbx
    mov [rsp + GF_RCX], rcx
    mov [rsp + GF_RDX], rdx
    mov [rsp + GF_RSI], rsi
    mov [rsp + GF_RDI], rdi
    mov [rsp + GF_RBP], rbp
    mov [rsp + GF_R8], r8
    mov [rsp + GF_R9], r9
    mov [rsp + GF_R10], r10
    mov [rsp + GF_R11], r11
    mov [rsp + GF_R12], r12
    mov [rsp + GF_R13], r13
    mov [rsp + GF_R14], r14
    mov [rsp + GF_R15], r15

    mov qword [rsp + GF_VECTOR], %2

    ; Copy exception frame (no error code: RIP at [rsp + GF_SIZE])
    lea r11, [rsp + GF_SIZE]
    mov r10, [r11]          ; RIP
    mov [rsp + GF_RIP], r10
    mov r10, [r11 + 8]      ; CS
    mov [rsp + GF_CS], r10
    mov r10, [r11 + 16]     ; RFLAGS
    mov [rsp + GF_RFLAGS], r10
    mov r10, [r11 + 24]     ; RSP
    mov [rsp + GF_RSP], r10
    mov r10, [r11 + 32]     ; SS
    mov [rsp + GF_SS], r10

    mov rdi, rsp
    sub rsp, 8              ; align for call
    call generic_fault_with_regs
    add rsp, 8

    ; RAX = null (kernel fault — halt) or context pointer (switch)
    test rax, rax
    jz %%halt

    ; Context switch to next thread via BSP_STACK
    mov r10, rax
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax
    mov rsp, [gs:0x08]      ; BSP_STACK from PerCpuData.kernel_rsp

    mov rax, [r10 + CONTEXT_CS]
    test al, 0x3
    jz %%kernel_frame

    ; User-mode target
    mov rax, [r10 + CONTEXT_SS]
    mov rbx, [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    mov rdx, [r10 + CONTEXT_CS]
    mov rsi, [r10 + CONTEXT_RIP]
    push rax                ; SS
    push rbx                ; RSP
    push rcx                ; RFLAGS
    push rdx                ; CS
    push rsi                ; RIP
    swapgs
    jmp shared_restore_regs

%%kernel_frame:
    push qword [r10 + CONTEXT_SS]
    push qword [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    push rcx
    push qword [r10 + CONTEXT_CS]
    push qword [r10 + CONTEXT_RIP]
    jmp shared_restore_regs

%%halt:
    cli
    hlt
    jmp %%halt
%endmacro

GENERIC_FAULT_ENTRY generic_fault_entry_de, 0   ; #DE Divide Error
GENERIC_FAULT_ENTRY generic_fault_entry_ud, 6   ; #UD Invalid Opcode
GENERIC_FAULT_ENTRY generic_fault_entry_of, 4   ; #OF Overflow
GENERIC_FAULT_ENTRY generic_fault_entry_br, 5   ; #BR Bound Range Exceeded
GENERIC_FAULT_ENTRY generic_fault_entry_nm, 7   ; #NM Device Not Available
GENERIC_FAULT_ENTRY generic_fault_entry_mf, 16  ; #MF x87 Floating-Point
GENERIC_FAULT_ENTRY generic_fault_entry_xm, 19  ; #XM SIMD Floating-Point

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
    ; Kernel-mode fault: x86_64 still pushes RSP/SS on the exception frame
    mov r10, [r11 + 32]     ; RSP (pushed by CPU even for same-privilege)
    mov [rsp + PF_RSP], r10
    mov r10, [r11 + 40]     ; SS
    mov [rsp + PF_SS], r10
.pf_have_user:

    mov rdi, rsp
    sub rsp, 8              ; align for call
    call pf_with_regs
    add rsp, 8

    ; Diagnostic: emit 'R' to COM2 after pf_with_regs returns
    push rax
    push rdx
    mov dx, 0x2F8
.pf_wait_tx_r:
    add dx, 5          ; 0x2FD = LSR
    in al, dx
    test al, 0x20      ; THRE bit
    jz .pf_wait_tx_r
    sub dx, 5
    mov al, 'R'
    out dx, al
    pop rdx
    pop rax

    ; RAX = null (resume via iretq), 0x1 (idle sentinel), or valid context ptr
    test rax, rax
    jz .pf_resume

    ; Check for idle sentinel (0x1) — fault was forwarded, delegate to timer
    cmp rax, 1
    je .pf_idle_loop

    ; ── Context switch to next thread ──
    ; Transfer from IST2 stack to BSP_STACK (kernel stack) and use the
    ; same restore path as the timer interrupt.  Direct IRETQ from IST2
    ; silently fails (mov ds,0x2b faults; skipping DS/ES causes timer to
    ; stop firing).  Using BSP_STACK avoids both issues.
    mov r10, rax

    ; Switch address space
    mov rax, [r10 + CONTEXT_CR3]
    mov cr3, rax

    ; Load BSP_STACK top from PerCpuData.kernel_rsp (gs:[0x08]).
    ; GS is still kernel GS (PF entry swapped user→kernel, context-switch
    ; SWAPGS hasn't happened yet).
    mov rsp, [gs:0x08]

    ; Build iretq frame for target thread on BSP_STACK
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
    jmp shared_restore_regs ; Use timer's proven restore path on BSP_STACK

.pf_build_kernel_frame:
    push qword [r10 + CONTEXT_SS]
    push qword [r10 + CONTEXT_RSP]
    mov rcx, [r10 + CONTEXT_RFLAGS]
    and rcx, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
    or  rcx, (1 << 9) | (1 << 1)
    push rcx
    push qword [r10 + CONTEXT_CS]
    push qword [r10 + CONTEXT_RIP]
    jmp shared_restore_regs

.pf_idle_loop:
    ; ── Fault forwarded: enter kernel idle loop on BSP_STACK ──
    ; The timer will preempt this and do a proper context switch.
    ; GS is kernel GS (PF entry swapped user→kernel).
    ; Load BSP_STACK from PerCpuData.kernel_rsp (gs:[0x08]).
    mov rsp, [gs:0x08]

    ; Diagnostic: emit RSP status to COM2
    ; 'K' = valid kernel address (top byte >= 0xFF), '0' = zero RSP
    push rax
    push rdx
    mov rdx, 0x2F8
.pf_wait_tx_s:
    mov eax, 0
    add dx, 5
    in al, dx
    sub dx, 5
    test al, 0x20
    jz .pf_wait_tx_s
    ; Check RSP validity (should be > 16 since we just pushed 2 things)
    lea rax, [rsp + 16]    ; original RSP before pushes
    test rax, rax
    jz .pf_rsp_zero
    mov al, 'K'            ; valid (non-zero)
    jmp .pf_rsp_emit
.pf_rsp_zero:
    mov al, '0'
.pf_rsp_emit:
    out dx, al
    pop rdx
    pop rax

    sti
.pf_idle_spin:
    hlt

    ; Diagnostic: emit 'W' to COM2 after waking from HLT
    mov dx, 0x2F8
.pf_wait_tx2:
    add dx, 5
    in al, dx
    test al, 0x20
    jz .pf_wait_tx2
    sub dx, 5
    mov al, 'W'
    out dx, al

    jmp .pf_idle_spin

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
    jmp shared_restore_regs

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

shared_restore_regs:
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
