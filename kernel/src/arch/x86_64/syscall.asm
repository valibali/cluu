; System Call Entry for x86_64
;
; This module implements the syscall entry point for x86_64 architecture.
; When userspace executes the SYSCALL instruction, CPU jumps here.
;
; Calling Convention (matches Linux/AMD64 ABI):
; - RAX: Syscall number
; - RDI: Argument 1
; - RSI: Argument 2
; - RDX: Argument 3
; - R10: Argument 4 (R10 used instead of RCX, as RCX holds return RIP)
; - R8:  Argument 5
; - R9:  Argument 6
;
; On syscall entry:
; - RCX: User RIP (return address)
; - R11: User RFLAGS
; - CS/SS: Switched to kernel segments
; - RSP: Still userspace stack (UNSAFE!)
;
; Security Requirements:
; 1. Immediately switch to kernel stack (from TSS.RSP0)
; 2. Save complete user context
; 3. Validate all arguments before use
; 4. Return to userspace with SYSRET

BITS 64
SECTION .text

; External symbols from Rust code
extern syscall_handler_rust

; Export syscall entry point
global syscall_entry

; Syscall Entry Point
;
; This is where the CPU jumps when userspace executes SYSCALL.
; At entry:
; - RCX = user RIP (return address)
; - R11 = user RFLAGS
; - RAX = syscall number
; - RDI, RSI, RDX, R10, R8, R9 = arguments
; - RSP = still userspace stack (MUST switch immediately!)
;
; We need to:
; 1. Switch to kernel stack
; 2. Save user context
; 3. Call Rust syscall handler
; 4. Restore user context
; 5. Return to userspace with SYSRET
syscall_entry:
    ; ===== CRITICAL: Switch to Kernel Stack =====
    ; At this point, RSP is still the userspace stack, which is UNTRUSTED.
    ; We MUST switch to the kernel stack immediately before any push operations.
    ;
    ; TODO: For full per-CPU support, we need:
    ; - SWAPGS to access per-CPU data via GS.base
    ; - IA32_KERNEL_GS_BASE MSR pointing to per-CPU structure
    ; - Per-CPU kernel stack allocation
    ;
    ; For now, we use a simple approach with a temporary location
    ; to save user RSP and a static kernel stack.

    ; Save user RSP to temporary storage
    mov qword [syscall_user_rsp], rsp

    ; Load kernel stack
    mov rsp, qword [syscall_kernel_stack_top]

    ; ===== Save User Context =====
    ; We need to save:
    ; - All general purpose registers
    ; - RCX (user RIP)
    ; - R11 (user RFLAGS)
    ; - Original RSP (user stack pointer)
    ;
    ; Stack layout after save (growing downward):
    ; [RSP+0]   = R11 (user RFLAGS)
    ; [RSP+8]   = RCX (user RIP)
    ; [RSP+16]  = RBX
    ; [RSP+24]  = RBP
    ; [RSP+32]  = R12
    ; [RSP+40]  = R13
    ; [RSP+48]  = R14
    ; [RSP+56]  = R15
    ; [RSP+64]  = User RSP
    ; [RSP+72]  = RAX (syscall number)
    ; [RSP+80]  = RDI (arg1)
    ; [RSP+88]  = RSI (arg2)
    ; [RSP+96]  = RDX (arg3)
    ; [RSP+104] = R10 (arg4)
    ; [RSP+112] = R8  (arg5)
    ; [RSP+120] = R9  (arg6)

    push r11                        ; Save user RFLAGS
    push rcx                        ; Save user RIP

    ; Save callee-saved registers (we must preserve these)
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15

    ; Save user RSP (from temporary storage)
    push qword [syscall_user_rsp]  ; Push saved user RSP

    ; Save syscall arguments
    push rax                        ; Syscall number
    push rdi                        ; Argument 1
    push rsi                        ; Argument 2
    push rdx                        ; Argument 3
    push r10                        ; Argument 4 (note: R10, not RCX)
    push r8                         ; Argument 5
    push r9                         ; Argument 6

    ; ===== Call Rust Syscall Handler =====
    ; Prepare arguments for syscall_handler_rust:
    ; - RDI: syscall number (RAX)
    ; - RSI: pointer to SyscallArgs structure
    ;
    ; SyscallArgs layout:
    ; struct SyscallArgs {
    ;     arg1: usize,  // RDI
    ;     arg2: usize,  // RSI
    ;     arg3: usize,  // RDX
    ;     arg4: usize,  // R10
    ;     arg5: usize,  // R8
    ;     arg6: usize,  // R9
    ; }

    mov rdi, rax                    ; First argument: syscall number
    mov rsi, rsp                    ; Second argument: pointer to args on stack
    add rsi, 8                      ; Skip RAX to point to arg1

    ; Align stack to 16 bytes for function call (required by System V ABI)
    and rsp, ~0xF

    ; Call the Rust handler
    ; fn syscall_handler_rust(number: usize, args: *const SyscallArgs) -> isize
    call syscall_handler_rust

    ; ===== Restore User Context =====
    ; RAX now contains the syscall return value (or negative errno)
    ; We need to restore all saved registers and return to userspace.

    ; Get saved user RSP
    mov r15, qword [syscall_user_rsp]  ; Temporary: use r15 to hold user RSP

    ; Restore stack pointer to where we saved context
    lea rsp, [rsp + (16 * 8)]       ; Skip to start of saved context (past alignment)

    ; Pop arguments (discard, we don't need them anymore)
    add rsp, 56                     ; Skip R9, R8, R10, RDX, RSI, RDI, RAX (7 * 8 = 56)

    ; Skip user RSP (we'll restore it separately)
    add rsp, 8

    ; Restore callee-saved registers
    ; Note: We skip r15 because we're using it to hold user RSP
    add rsp, 8                      ; Skip saved r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx

    ; Restore RCX and R11 for SYSRET
    pop rcx                         ; User RIP
    pop r11                         ; User RFLAGS

    ; Restore user RSP (from r15 temp)
    mov rsp, r15                    ; Restore original user RSP

    ; ===== Return to Userspace =====
    ; SYSRET instruction:
    ; - Sets RIP = RCX (user return address)
    ; - Sets RFLAGS = R11 (user flags)
    ; - Sets CS = IA32_STAR[63:48] + 16 (user code segment)
    ; - Sets SS = IA32_STAR[63:48] + 8  (user data segment)
    ; - RAX contains return value
    sysret

; Syscall Stub for Error Cases
;
; If syscall entry is called without proper setup, return an error.
global syscall_stub
syscall_stub:
    mov rax, -38                    ; Return -ENOSYS
    ret

SECTION .data
; Temporary storage for user RSP during syscall
; TODO: Replace with per-CPU data when we have proper SMP support
syscall_user_rsp: dq 0

SECTION .bss
; Kernel syscall stack (16KB)
; TODO: Replace with per-CPU stacks when we have proper SMP support
align 16
syscall_kernel_stack: resb 16384
syscall_kernel_stack_top:
