; ═══════════════════════════════════════════════════════════════════════════
; CLUU Microkernel - Syscall Entry Point (x86_64)
; ═══════════════════════════════════════════════════════════════════════════
;
; This file implements the low-level syscall entry/exit for the SYSCALL/SYSRET
; instructions on x86_64.
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
; # SYSRET Instruction Behavior
;
; The SYSRETQ instruction does:
;   RIP ← RCX
;   RFLAGS ← R11
;   CS ← IA32_STAR[63:48] + 16
;   SS ← IA32_STAR[63:48] + 8

[BITS 64]
section .text

global syscall_entry
extern syscall_dispatch
extern schedule_and_switch

; ═══════════════════════════════════════════════════════════════════════════
; Syscall Entry Point
; ═══════════════════════════════════════════════════════════════════════════

syscall_entry:
    ; At this point:
    ; - RCX = user RIP (return address)
    ; - R11 = user RFLAGS
    ; - CS/SS switched to kernel segments
    ; - Still using user stack!

    ; ───────────────────────────────────────────────────────────────────────
    ; Switch to kernel stack
    ; ───────────────────────────────────────────────────────────────────────

    swapgs                          ; Switch to kernel GS base
                                    ; (GS now points to per-CPU data)

    mov [gs:0x00], rsp              ; Save user RSP to per-CPU area
    mov rsp, [gs:0x08]              ; Load kernel RSP from per-CPU area

    ; ───────────────────────────────────────────────────────────────────────
    ; Save user context on kernel stack
    ; ───────────────────────────────────────────────────────────────────────

    ; Save registers that must be preserved across function calls
    ; and registers modified by SYSCALL instruction

    push rcx                        ; User RIP (return address)
    push r11                        ; User RFLAGS

    ; Save callee-saved registers (System V AMD64 ABI)
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15

    ; ───────────────────────────────────────────────────────────────────────
    ; Prepare arguments for syscall_dispatch
    ; ───────────────────────────────────────────────────────────────────────

    ; Arguments are already in correct registers for System V ABI:
    ; RDI = syscall number (arg1) - from RAX
    ; RSI = arg2 (already in RSI)
    ; RDX = arg3 (already in RDX)
    ; RCX = arg4 (need to move from R10)
    ; R8  = arg5 (already in R8)
    ; R9  = arg6 (already in R9)

    mov rdi, rax                    ; Move syscall number to first arg
    mov rcx, r10                    ; Move arg4 from R10 to RCX
                                    ; (R10 used instead of RCX because
                                    ;  SYSCALL clobbers RCX)

    ; ───────────────────────────────────────────────────────────────────────
    ; Call Rust dispatcher
    ; ───────────────────────────────────────────────────────────────────────

    ; extern "C" fn syscall_dispatch(
    ;     number: usize,  // RDI
    ;     arg1: usize,    // RSI
    ;     arg2: usize,    // RDX
    ;     arg3: usize,    // RCX
    ;     arg4: usize,    // R8
    ;     arg5: usize,    // R9
    ;     arg6: usize,    // [rsp] (7th arg goes on stack)
    ; ) -> isize;

    ; Note: We have 6 register args, so no stack args needed
    ; Stack is already aligned (we pushed even number of qwords)

    ; Re-enable interrupts now that we're safely on kernel stack
    ; SYSCALL disabled interrupts (via FMASK), but now it's safe
    ; This allows:
    ; - Timer interrupts during syscalls
    ; - Blocking syscalls to yield CPU properly
    ; - Scheduler to preempt long-running syscalls
    sti

    call syscall_dispatch

    ; Return value in RAX (already there from function return)
    ; Save it - we'll need it after potential context switch
    push rax

    ; ───────────────────────────────────────────────────────────────────────
    ; seL4 Fastpath: Context Switch Check
    ; ───────────────────────────────────────────────────────────────────────
    ;
    ; Build Context structure on stack for schedule_and_switch:
    ; Offset  Size  Field
    ; ------  ----  -----
    ; 0x00    8     RBX
    ; 0x08    8     RBP
    ; 0x10    8     R12
    ; 0x18    8     R13
    ; 0x20    8     R14
    ; 0x28    8     R15
    ; 0x30    8     RSP (user)
    ; 0x38    8     RIP (user, from RCX)
    ; 0x40    8     RFLAGS (user, from R11)
    ; 0x48    8     CS (user code segment)
    ; 0x50    8     SS (user data segment)
    ; 0x58    8     CR3 (page table root)
    ; Total: 96 bytes (0x60)

    ; Stack currently has (top to bottom):
    ; [RSP+0]  = syscall return value (RAX, just pushed)
    ; [RSP+8]  = R15
    ; [RSP+16] = R14
    ; [RSP+24] = R13
    ; [RSP+32] = R12
    ; [RSP+40] = RBX
    ; [RSP+48] = RBP
    ; [RSP+56] = R11 (user RFLAGS)
    ; [RSP+64] = RCX (user RIP)

    ; Build Context by pushing in reverse order (highest offset first)
    sub rsp, 96                     ; Reserve space for Context

    ; Get CR3 and store at offset 0x58
    mov r8, cr3
    mov [rsp + 0x58], r8

    ; User segments (constants) at offsets 0x50 and 0x48
    mov qword [rsp + 0x50], 0x2b    ; SS (user data segment, index 5, RPL 3)
    mov qword [rsp + 0x48], 0x33    ; CS (user code segment, index 6, RPL 3)

    ; RFLAGS (from R11, saved at RSP+96+56)
    mov r8, [rsp + 152]             ; 96 (context) + 8 (retval) + 48 = 152
    mov [rsp + 0x40], r8

    ; RIP (from RCX, saved at RSP+96+64)
    mov r8, [rsp + 160]             ; 96 + 8 + 56 = 160
    mov [rsp + 0x38], r8

    ; User RSP (from GS:0x00)
    mov r8, [gs:0x00]
    mov [rsp + 0x30], r8

    ; Callee-saved registers (from stack)
    mov r8, [rsp + 104]             ; R15 at RSP+96+8
    mov [rsp + 0x28], r8

    mov r8, [rsp + 112]             ; R14
    mov [rsp + 0x20], r8

    mov r8, [rsp + 120]             ; R13
    mov [rsp + 0x18], r8

    mov r8, [rsp + 128]             ; R12
    mov [rsp + 0x10], r8

    mov r8, [rsp + 136]             ; RBP
    mov [rsp + 0x08], r8

    mov r8, [rsp + 144]             ; RBX
    mov [rsp + 0x00], r8

    ; Now [RSP] points to a complete Context structure
    ; Call schedule_and_switch(context_ptr)
    mov rdi, rsp
    call schedule_and_switch

    ; RAX = pointer to next thread's Context (or NULL if no switch)
    test rax, rax
    jz .no_context_switch

    ; ───────────────────────────────────────────────────────────────────────
    ; Context Switch: Load next thread's context and SYSRET to it
    ; ───────────────────────────────────────────────────────────────────────

    ; Load callee-saved registers
    mov rbx, [rax + 0x00]
    mov rbp, [rax + 0x08]
    mov r12, [rax + 0x10]
    mov r13, [rax + 0x18]
    mov r14, [rax + 0x20]
    mov r15, [rax + 0x28]

    ; Load RIP and RFLAGS for SYSRET
    mov rcx, [rax + 0x38]           ; User RIP
    mov r11, [rax + 0x40]           ; User RFLAGS

    ; Load CR3 (switch page tables)
    mov r8, [rax + 0x58]
    mov cr3, r8

    ; Load user RSP
    mov r8, [rax + 0x30]
    mov [gs:0x00], r8               ; Save in per-CPU area
    mov rsp, r8                     ; Load into RSP

    ; SWAPGS and return to userspace
    swapgs
    cli                             ; Disable interrupts before SYSRET

    ; Syscall return value for new thread is 0 (successful yield)
    xor rax, rax

    o64 sysret

.no_context_switch:
    ; ───────────────────────────────────────────────────────────────────────
    ; No context switch - restore original thread's context
    ; ───────────────────────────────────────────────────────────────────────

    ; Clean up Context structure
    add rsp, 96

    ; Restore syscall return value
    pop rax

    ; Restore callee-saved registers
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp

    ; Restore registers for SYSRET
    pop r11                         ; User RFLAGS
    pop rcx                         ; User RIP

    ; ───────────────────────────────────────────────────────────────────────
    ; Switch back to user stack
    ; ───────────────────────────────────────────────────────────────────────

    mov rsp, [gs:0x00]              ; Restore user RSP
    swapgs                          ; Switch back to user GS

    ; ───────────────────────────────────────────────────────────────────────
    ; Return to userspace
    ; ───────────────────────────────────────────────────────────────────────

    ; Disable interrupts before SYSRET
    ; This prevents interrupts during the SYSRET instruction itself
    ; User RFLAGS (in R11) will re-enable interrupts when restored
    cli

    ; SYSRETQ does:
    ; - RIP ← RCX (user return address)
    ; - RFLAGS ← R11 (user RFLAGS, will re-enable interrupts)
    ; - CS ← IA32_STAR[63:48] + 16 (user code segment)
    ; - SS ← IA32_STAR[63:48] + 8  (user data segment)

    o64 sysret                      ; Return to userspace

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
; We push an even number of qwords (8 qwords = 64 bytes) before calling
; syscall_dispatch, ensuring alignment.
;
; # Security Considerations
;
; - User RSP is saved to per-CPU area (not user-accessible)
; - Kernel stack is separate from user stack
; - GS base switched atomically with SWAPGS
; - All user registers validated/sanitized by Rust handlers
;
; # Performance
;
; - Fast path: ~50 cycles for minimal syscall (yield)
; - SYSCALL/SYSRET faster than INT/IRET (no privilege check)
; - Per-CPU data avoids memory barriers
