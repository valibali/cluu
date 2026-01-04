# Phase 8b: Token System Refactor

## Overview

Refactor the existing capability system into a secure token-based authorization system with JWT-like semantics. This phase transforms the kernel's authorization model to use unforgeable tokens with opaque scopes, explicit delegation, and mandatory expiration.

## Goals

1. **Minimal syscall surface** - Reduce syscall count from 14 to 7
2. **Secure by design** - Opaque, non-enumerable tokens prevent probing
3. **Explicit delegation** - Enable seL4-style authority derivation
4. **Time-bounded security** - Mandatory expiration limits token lifetime
5. **Cryptographic integrity** - HMAC signatures prevent forgery

---

## New Syscall Interface

### Syscall Numbers

```rust
#[repr(usize)]
pub enum SyscallNumber {
    Send = 0,        // Send IPC message
    Recv = 1,        // Receive IPC message
    Call = 2,        // Send + Receive (synchronous RPC)
    Reply = 3,       // Reply to IPC sender
    Yield = 4,       // Give up CPU
    Invoke = 5,      // Invoke operation on a token
    DebugPrint = 255,
}
```

**7 syscalls total** (down from 14)

### Syscall Semantics

```rust
// IPC syscalls
sys_send(token, msg_ptr, msg_len, 0, 0, 0) -> Result<(), Error>
sys_recv(token, buf_ptr, buf_len, 0, 0, 0) -> Result<usize, Error>
sys_call(token, msg_ptr, msg_len, buf_ptr, buf_len, 0) -> Result<usize, Error>
sys_reply(msg_ptr, msg_len, 0, 0, 0, 0) -> Result<(), Error>

// Scheduling
sys_yield(0, 0, 0, 0, 0, 0) -> Result<(), Error>

// Token operations (all operations go through invoke)
sys_invoke(token_handle, operation, arg1, arg2, arg3, arg4) -> Result<usize, Error>

// Debug (only in debug builds)
sys_debug_print(msg_ptr, msg_len, 0, 0, 0, 0) -> Result<(), Error>
```

---

## Token Format

### Structure

```rust
/// Token = unforgeable authority to perform operations on an object
///
/// Analogous to JWT with:
/// - scope = sub (subject)
/// - role = permissions
/// - issuer = iss (issuer)
/// - expire_at = exp (expiration)
/// - signature = JWT signature
pub struct Token {
    /// Opaque, non-enumerable reference to kernel object
    ///
    /// Security properties:
    /// - Cannot enumerate to discover objects
    /// - Cannot forge or guess valid scopes
    /// - Kernel-internal mapping to actual objects
    scope: OpaqueScope,

    /// Rights mask (NOT role-based access control)
    ///
    /// Explicit bit-level permissions prevent privilege confusion.
    /// Clear semantics: each bit = specific operation allowed.
    role: Rights,

    /// Identifies delegation domain
    ///
    /// - `Issuer::Kernel` = kernel-minted root authority
    /// - `Issuer::Authority(id)` = userspace service-minted derived token
    ///
    /// Enables seL4-style pattern: servers mint tokens for resources they manage.
    issuer: Issuer,

    /// Mandatory expiration timestamp
    ///
    /// All serialized tokens MUST have expiration.
    /// Benefits:
    /// - Bounds replay window
    /// - Reduces need for revocation lists
    /// - Forces re-authentication for long-lived access
    ///
    /// Kernel maintains monotonic timestamp.
    expire_at: Timestamp,

    /// HMAC signature binding all authorization-relevant fields
    ///
    /// signature = HMAC-SHA256(scope || role || issuer || expire_at, kernel_secret)
    ///
    /// Properties:
    /// - No field can be modified without breaking signature
    /// - Only kernel can create valid signatures
    /// - Userspace cannot forge tokens
    signature: Signature,
}
```

### Field Details

#### 1. Opaque Scope

```rust
/// Opaque identifier for kernel objects
///
/// Properties:
/// - Non-enumerable: Cannot iterate to discover objects
/// - Unforgeable: Random 128-bit value, not sequential
/// - Unlinkable: Same object gets different scope in different tokens
#[derive(Clone, Copy)]
pub struct OpaqueScope([u8; 16]); // 128-bit random ID

/// Kernel-internal mapping
struct ScopeTable {
    // Maps opaque scope -> actual object reference
    scopes: HashMap<OpaqueScope, ObjectRef>,
}

enum ObjectRef {
    Thread(ThreadId),
    Space(AddressSpaceId),
    Endpoint(EndpointId),
    Irq(IrqNumber),
}
```

**Security benefit**: Prevents probing attacks where attacker tries sequential IDs to discover objects.

#### 2. Rights Mask

```rust
/// Rights bitmask - explicit permissions
///
/// NOT role-based (no "admin", "user" roles).
/// Each bit = specific operation allowed.
bitflags! {
    pub struct Rights: u32 {
        // Generic rights
        const READ     = 1 << 0;  // Read object state
        const WRITE    = 1 << 1;  // Modify object state
        const DESTROY  = 1 << 2;  // Destroy object

        // Delegation
        const GRANT    = 1 << 3;  // Create derived tokens

        // Thread-specific
        const THREAD_CONTROL = 1 << 8;  // Modify thread (priority, etc.)
        const THREAD_SUSPEND = 1 << 9;  // Suspend/resume thread

        // Space-specific
        const SPACE_MAP    = 1 << 16; // Map pages
        const SPACE_UNMAP  = 1 << 17; // Unmap pages
        const SPACE_GRANT  = 1 << 18; // Grant pages to other spaces

        // IPC-specific
        const IPC_SEND     = 1 << 24; // Send to endpoint
        const IPC_RECV     = 1 << 25; // Receive from endpoint
        const IPC_CALL     = 1 << 26; // Call (send+recv)

        // IRQ-specific
        const IRQ_HANDLE   = 1 << 28; // Handle IRQ
        const IRQ_ACK      = 1 << 29; // Acknowledge IRQ
    }
}
```

**Security benefit**: No ambiguity about what operations are allowed. Prevents privilege escalation through role confusion.

#### 3. Issuer

```rust
/// Token issuer - identifies delegation domain
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Issuer {
    /// Kernel-minted root authority
    ///
    /// These tokens are created by the kernel during boot
    /// and represent fundamental authority.
    Kernel,

    /// Userspace authority (server ID)
    ///
    /// Userspace services can mint derived tokens for resources
    /// they manage. Example: VFS server mints file tokens.
    Authority(AuthorityId),
}

/// Authority identifier (userspace service)
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AuthorityId(u64);
```

**Use case**: VFS server receives root token for its address space, then mints file descriptor tokens with restricted rights for client processes.

#### 4. Expiration

```rust
/// Monotonic timestamp (nanoseconds since boot)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Token {
    /// Check if token is expired
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now >= self.expire_at
    }
}
```

**Policy**:
- Root tokens: Long expiration (years)
- Derived tokens: Short expiration (seconds to minutes)
- Serialized tokens: Mandatory expiration enforced

**Security benefit**: Limits damage from token theft. Stolen token becomes useless after expiration.

#### 5. Signature

```rust
/// HMAC-SHA256 signature (32 bytes)
#[derive(Clone, Copy)]
pub struct Signature([u8; 32]);

impl Token {
    /// Compute signature over token fields
    fn compute_signature(
        scope: OpaqueScope,
        role: Rights,
        issuer: Issuer,
        expire_at: Timestamp,
        secret: &[u8; 32],
    ) -> Signature {
        // signature = HMAC-SHA256(scope || role || issuer || expire_at, kernel_secret)
        let mut hasher = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        hasher.update(&scope.0);
        hasher.update(&role.bits().to_le_bytes());
        hasher.update(&issuer.to_bytes());
        hasher.update(&expire_at.0.to_le_bytes());
        Signature(hasher.finalize().into_bytes().into())
    }

    /// Verify token signature
    pub fn verify(&self, secret: &[u8; 32]) -> bool {
        let expected = Self::compute_signature(
            self.scope,
            self.role,
            self.issuer,
            self.expire_at,
            secret,
        );
        // Constant-time comparison to prevent timing attacks
        constant_time_eq(&expected.0, &self.signature.0)
    }
}
```

**Security benefit**: Prevents forgery. Only kernel (with secret key) can create valid tokens.

---

## Token Operations

### sys_invoke() Operations

```rust
/// Operations performed through sys_invoke()
#[repr(usize)]
pub enum InvokeOp {
    // Thread operations
    ThreadCreate = 0,    // Create new thread
    ThreadDestroy = 1,   // Destroy thread
    ThreadSuspend = 2,   // Suspend thread
    ThreadResume = 3,    // Resume thread
    ThreadSetPriority = 4, // Change priority

    // Space operations
    SpaceCreate = 10,    // Create address space
    SpaceDestroy = 11,   // Destroy address space
    SpaceMap = 12,       // Map page
    SpaceUnmap = 13,     // Unmap page
    SpaceGrant = 14,     // Grant page to another space

    // Token operations
    TokenDerive = 20,    // Create derived token with reduced rights
    TokenRevoke = 21,    // Revoke derived token

    // IRQ operations
    IrqAttach = 30,      // Attach IRQ to endpoint
    IrqAck = 31,         // Acknowledge IRQ
}
```

### Examples

```rust
// Create address space (requires token with SPACE_CREATE right)
let space_token_handle = sys_invoke(
    root_token_handle,
    InvokeOp::SpaceCreate,
    0, 0, 0, 0
)?;

// Map page into space (requires token with SPACE_MAP right)
sys_invoke(
    space_token_handle,
    InvokeOp::SpaceMap,
    virt_addr,
    phys_addr,
    flags,
    0
)?;

// Create thread in space (requires token with THREAD_CREATE right)
let thread_token_handle = sys_invoke(
    space_token_handle,
    InvokeOp::ThreadCreate,
    entry_point,
    stack_pointer,
    priority,
    0
)?;

// Derive restricted token (requires token with GRANT right)
// Example: Create read-only token from read-write token
let ro_token_handle = sys_invoke(
    rw_token_handle,
    InvokeOp::TokenDerive,
    Rights::READ.bits(),  // Reduced rights (no WRITE)
    expire_timestamp,     // Shorter expiration
    target_thread_id,     // Who receives the token
    0
)?;
```

---

## Token Lifecycle

### 1. Kernel-Minted Root Tokens

Created during boot for initial processes:

```rust
fn boot_create_init_tokens() {
    // Root space token with full rights
    let root_space_token = Token::new(
        OpaqueScope::random(),
        Rights::all(),           // All rights
        Issuer::Kernel,
        Timestamp::MAX,          // Never expires (root token)
        kernel_secret,
    );

    // Give to init process
    init_process.add_token(root_space_token);
}
```

### 2. Userspace-Minted Derived Tokens

Userspace services mint tokens for resources they manage:

```rust
// In VFS server:
fn open_file(&mut self, path: &str, flags: OpenFlags) -> Result<TokenHandle, Error> {
    // VFS has root token for its space
    // Mint a new token for this file descriptor with reduced rights

    let file_rights = if flags.contains(O_RDWR) {
        Rights::READ | Rights::WRITE
    } else if flags.contains(O_WRONLY) {
        Rights::WRITE
    } else {
        Rights::READ
    };

    // Create derived token
    let file_token = sys_invoke(
        self.root_token,
        InvokeOp::TokenDerive,
        file_rights.bits(),
        now() + FILE_TOKEN_LIFETIME, // 5 minute expiration
        current_thread_id(),
        0
    )?;

    Ok(file_token)
}
```

### 3. Token Passing via IPC

Tokens can be transferred between processes:

```rust
// Send token in IPC message
struct IpcMessage {
    data: [u8; 256],
    tokens: [Option<TokenHandle>; 4], // Up to 4 tokens per message
}

// Sender grants token to receiver
sys_send(endpoint_token, &msg_with_token, size, 0, 0, 0)?;

// Receiver gets token in message
let (bytes_received, received_tokens) = sys_recv(endpoint_token, &mut buf, size, 0, 0, 0)?;
```

### 4. Token Expiration

Kernel checks expiration on every use:

```rust
fn invoke_token(handle: TokenHandle, op: InvokeOp, args: &[usize]) -> SyscallResult {
    let token = lookup_token(handle)?;

    // Check expiration
    if token.is_expired(current_timestamp()) {
        return Err(Error::TokenExpired);
    }

    // Check signature
    if !token.verify(&KERNEL_SECRET) {
        return Err(Error::InvalidSignature);
    }

    // Check rights
    if !token.has_right_for_operation(op) {
        return Err(Error::InsufficientRights);
    }

    // Perform operation
    perform_operation(&token, op, args)
}
```

---

## Migration Plan

### Phase 1: Token Infrastructure

1. **Implement Token structure** (`kernel/src/token/mod.rs`)
   - OpaqueScope with random generation
   - Rights bitmask
   - Issuer enum
   - Timestamp handling
   - HMAC signature computation and verification

2. **Token table** (`kernel/src/token/table.rs`)
   - TokenHandle → Token mapping
   - Opaque scope → Object reference mapping
   - Token lifecycle (create, revoke, garbage collect)

3. **Crypto primitives** (`kernel/src/crypto/`)
   - HMAC-SHA256 implementation
   - Constant-time comparison
   - Random number generation for scopes

### Phase 2: Syscall Refactor

1. **Update syscall numbers** (`kernel/src/syscall/mod.rs`)
   - Remove old syscalls (ThreadCreate, SpaceCreate, Map, etc.)
   - Add new syscalls (Send, Recv, Call, Reply, Invoke)

2. **Implement sys_invoke()** (`kernel/src/syscall/handlers/invoke.rs`)
   - Dispatch to operation handlers based on InvokeOp
   - Token verification (expiration, signature, rights)
   - Operation execution

3. **Migrate operations** to sys_invoke()
   - Thread operations (create, destroy, suspend, resume)
   - Space operations (create, destroy, map, unmap)
   - Token operations (derive, revoke)

### Phase 3: Update Existing Code

1. **Thread management**
   - Remove direct Thread::new() calls
   - Use tokens for thread creation
   - Thread stores token that created it

2. **Address space management**
   - Require token for all space operations
   - Map/unmap through sys_invoke()

3. **Process management** (userspace)
   - Procmgr uses tokens for all operations
   - Mints derived tokens for child processes

### Phase 4: IPC Integration

1. **Endpoint tokens**
   - Each IPC endpoint requires token
   - Rights: IPC_SEND, IPC_RECV, IPC_CALL

2. **Token passing**
   - Send tokens in IPC messages
   - Kernel validates and transfers ownership

3. **Update IPC handlers**
   - sys_send(), sys_recv(), sys_call(), sys_reply()

### Phase 5: Testing and Validation

1. **Unit tests**
   - Token signature verification
   - Expiration checking
   - Rights enforcement

2. **Integration tests**
   - Token passing via IPC
   - Derived token creation
   - Operation authorization

3. **Security tests**
   - Cannot forge tokens
   - Cannot use expired tokens
   - Cannot escalate privileges

---

## Implementation Files

```
kernel/src/
├── token/
│   ├── mod.rs              # Token structure and core logic
│   ├── table.rs            # Token handle → Token mapping
│   ├── scope.rs            # OpaqueScope generation and mapping
│   ├── rights.rs           # Rights bitmask definitions
│   └── signature.rs        # HMAC signature computation
├── crypto/
│   ├── mod.rs              # Crypto exports
│   ├── hmac.rs             # HMAC-SHA256 implementation
│   └── random.rs           # Secure random number generation
├── syscall/
│   ├── mod.rs              # Updated syscall numbers
│   └── handlers/
│       ├── invoke.rs       # sys_invoke() implementation
│       ├── ipc.rs          # sys_send/recv/call/reply
│       └── yield.rs        # sys_yield()
├── arch/x86_64/
│   └── syscall.rs          # Syscall entry/exit assembly
└── sched/
    ├── thread.rs           # Thread with token references
    └── ...

libcluu/src/
├── syscall/
│   ├── mod.rs              # Userspace syscall interface
│   ├── raw.rs              # Raw syscall wrappers (inline asm)
│   ├── ipc.rs              # IPC syscall helpers
│   ├── invoke.rs           # Invoke syscall helpers
│   └── types.rs            # Shared types (Rights, etc.)
└── token/
    ├── mod.rs              # Token handle management
    └── types.rs            # Token types for userspace
```

---

## Syscall Assembly Interface

### x86_64 Syscall Entry (NASM)

**File: `kernel/src/arch/x86_64/syscall_entry.asm`**

```nasm
; Syscall convention (match Linux for compatibility):
; RAX = syscall number
; RDI = arg1
; RSI = arg2
; RDX = arg3
; R10 = arg4 (R10 instead of RCX because SYSCALL clobbers RCX)
; R8  = arg5
; R9  = arg6
; Return: RAX (positive = success, negative = -errno)

[BITS 64]
section .text

global syscall_entry
extern syscall_dispatch

syscall_entry:
    ; Save user stack pointer
    swapgs                      ; Switch to kernel GS
    mov [gs:0x00], rsp          ; Save user RSP to per-CPU area
    mov rsp, [gs:0x08]          ; Load kernel RSP from per-CPU area

    ; Save user context on kernel stack
    push rcx                    ; Return RIP (set by SYSCALL instruction)
    push r11                    ; Return RFLAGS (set by SYSCALL instruction)
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15

    ; Arguments are already in correct registers:
    ; RAX = syscall number
    ; RDI = arg1
    ; RSI = arg2
    ; RDX = arg3
    ; R10 = arg4
    ; R8  = arg5
    ; R9  = arg6

    ; Move R10 to RCX for C calling convention (arg4)
    mov rcx, r10

    ; Call Rust dispatcher
    ; syscall_dispatch(number: rax, arg1: rdi, arg2: rsi, arg3: rdx, arg4: rcx, arg5: r8, arg6: r9)
    call syscall_dispatch

    ; Return value in RAX

    ; Restore user context
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    pop r11                     ; Restore RFLAGS for SYSRET
    pop rcx                     ; Restore RIP for SYSRET

    ; Restore user stack pointer
    mov rsp, [gs:0x00]          ; Restore user RSP
    swapgs                      ; Switch back to user GS

    ; Return to userspace
    sysretq                     ; RCX -> RIP, R11 -> RFLAGS
```

**File: `kernel/src/arch/x86_64/syscall.rs`**

```rust
//! Syscall handling for x86_64

use crate::syscall::{SyscallNumber, SyscallArgs, dispatch_syscall};
use crate::error::Error;

// Import syscall_entry from NASM
extern "C" {
    pub fn syscall_entry();
}

/// Syscall dispatcher called from assembly
///
/// This function is called from syscall_entry.asm with arguments
/// already in the correct registers per x86_64 System V ABI.
#[no_mangle]
extern "C" fn syscall_dispatch(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    let syscall_num = match SyscallNumber::from_usize(number) {
        Some(n) => n,
        None => return -(Error::InvalidSyscall as isize),
    };

    let args = SyscallArgs::new(arg1, arg2, arg3, arg4, arg5, arg6);

    match dispatch_syscall(syscall_num, args) {
        Ok(ret) => ret as isize,
        Err(e) => -(e as isize),
    }
}

/// Initialize syscall handling
///
/// Sets up MSRs for SYSCALL/SYSRET instructions:
/// - IA32_STAR: Kernel/user CS selectors
/// - IA32_LSTAR: Syscall entry address
/// - IA32_FMASK: RFLAGS mask
pub fn init() {
    use x86_64::registers::model_specific::{LStar, Star, SFMask};
    use x86_64::VirtAddr;

    unsafe {
        // Set syscall entry point
        LStar::write(VirtAddr::new(syscall_entry as u64));

        // Set kernel/user code segment selectors
        // STAR[47:32] = Kernel CS (0x08)
        // STAR[63:48] = User CS - 16 (0x18, user CS = 0x1B, user SS = 0x23)
        Star::write(0x0008_u16, 0x0018_u16).unwrap();

        // Set RFLAGS mask (clear IF, DF, TF, AC, NT)
        SFMask::write(x86_64::registers::rflags::RFlags::INTERRUPT_FLAG);
    }

    klibcluu::info("Syscall interface initialized");
}
```

**Build integration in `kernel/Cargo.toml`:**

```toml
[build-dependencies]
cc = "1.0"

# In build.rs:
fn main() {
    // Compile NASM syscall entry
    cc::Build::new()
        .file("src/arch/x86_64/syscall_entry.asm")
        .flag("-f")
        .flag("elf64")
        .compile("syscall_entry");
}
```

---

## Syscall Handlers

### Invoke Handler (`kernel/src/syscall/handlers/invoke.rs`)

```rust
use crate::token::{Token, TokenHandle, InvokeOp, Rights};
use crate::error::Error;
use crate::syscall::{SyscallArgs, SyscallResult};

/// sys_invoke(token_handle, operation, arg1, arg2, arg3, arg4) -> Result<usize, Error>
///
/// Generic operation invocation on a token.
///
/// # Arguments
/// - arg1: token_handle - Handle to token
/// - arg2: operation - InvokeOp variant
/// - arg3-arg6: Operation-specific arguments
///
/// # Returns
/// - Ok(value) - Operation-specific return value (often new token handle or 0)
/// - Err(Error) - Error code
pub fn sys_invoke(args: SyscallArgs) -> SyscallResult {
    let token_handle = TokenHandle::from_raw(args.arg1);
    let operation = InvokeOp::from_usize(args.arg2)
        .ok_or(Error::InvalidOperation)?;

    // Lookup and validate token
    let token = TOKEN_TABLE.lookup(token_handle)?;

    // Check expiration
    if token.is_expired(current_timestamp()) {
        return Err(Error::TokenExpired);
    }

    // Verify signature
    if !token.verify(&KERNEL_SECRET) {
        return Err(Error::InvalidSignature);
    }

    // Dispatch based on operation
    match operation {
        // Thread operations
        InvokeOp::ThreadCreate => invoke_thread_create(&token, args.arg3, args.arg4, args.arg5),
        InvokeOp::ThreadDestroy => invoke_thread_destroy(&token, args.arg3),
        InvokeOp::ThreadSuspend => invoke_thread_suspend(&token, args.arg3),
        InvokeOp::ThreadResume => invoke_thread_resume(&token, args.arg3),
        InvokeOp::ThreadSetPriority => invoke_thread_set_priority(&token, args.arg3, args.arg4),

        // Space operations
        InvokeOp::SpaceCreate => invoke_space_create(&token),
        InvokeOp::SpaceDestroy => invoke_space_destroy(&token, args.arg3),
        InvokeOp::SpaceMap => invoke_space_map(&token, args.arg3, args.arg4, args.arg5),
        InvokeOp::SpaceUnmap => invoke_space_unmap(&token, args.arg3),
        InvokeOp::SpaceGrant => invoke_space_grant(&token, args.arg3, args.arg4, args.arg5),

        // Token operations
        InvokeOp::TokenDerive => invoke_token_derive(&token, args.arg3, args.arg4, args.arg5),
        InvokeOp::TokenRevoke => invoke_token_revoke(&token, args.arg3),

        // IRQ operations
        InvokeOp::IrqAttach => invoke_irq_attach(&token, args.arg3, args.arg4),
        InvokeOp::IrqAck => invoke_irq_ack(&token, args.arg3),
    }
}

// Example operation handler
fn invoke_space_map(
    token: &Token,
    virt_addr: usize,
    phys_addr: usize,
    flags: usize,
) -> SyscallResult {
    // Check rights
    if !token.role.contains(Rights::SPACE_MAP) {
        return Err(Error::InsufficientRights);
    }

    // Get space from token scope
    let space_id = TOKEN_TABLE.resolve_scope(&token.scope, ObjectType::Space)?;

    // Perform mapping
    SPACE_TABLE.with_space_mut(space_id, |space| {
        space.map_page(
            VirtAddr::new(virt_addr as u64),
            PhysAddr::new(phys_addr as u64),
            PageFlags::from_bits(flags),
        )
    })?;

    Ok(0)
}
```

### IPC Handlers (`kernel/src/syscall/handlers/ipc.rs`)

```rust
/// sys_send(endpoint_token, msg_ptr, msg_len, 0, 0, 0) -> Result<(), Error>
pub fn sys_send(args: SyscallArgs) -> SyscallResult {
    let endpoint_token = TokenHandle::from_raw(args.arg1);
    let msg_ptr = args.arg2 as *const u8;
    let msg_len = args.arg3;

    // Validate token
    let token = TOKEN_TABLE.lookup(endpoint_token)?;
    if !token.role.contains(Rights::IPC_SEND) {
        return Err(Error::InsufficientRights);
    }

    // Validate user pointer
    userptr::validate_read(msg_ptr, msg_len)?;

    // Get endpoint from scope
    let endpoint_id = TOKEN_TABLE.resolve_scope(&token.scope, ObjectType::Endpoint)?;

    // Copy message from userspace
    let msg = unsafe { core::slice::from_raw_parts(msg_ptr, msg_len) };

    // Send to endpoint
    IPC_MANAGER.send(endpoint_id, msg)?;

    Ok(0)
}

/// sys_recv(endpoint_token, buf_ptr, buf_len, 0, 0, 0) -> Result<usize, Error>
pub fn sys_recv(args: SyscallArgs) -> SyscallResult {
    let endpoint_token = TokenHandle::from_raw(args.arg1);
    let buf_ptr = args.arg2 as *mut u8;
    let buf_len = args.arg3;

    // Validate token
    let token = TOKEN_TABLE.lookup(endpoint_token)?;
    if !token.role.contains(Rights::IPC_RECV) {
        return Err(Error::InsufficientRights);
    }

    // Validate user pointer
    userptr::validate_write(buf_ptr, buf_len)?;

    // Get endpoint from scope
    let endpoint_id = TOKEN_TABLE.resolve_scope(&token.scope, ObjectType::Endpoint)?;

    // Receive from endpoint (blocking)
    let msg = IPC_MANAGER.recv(endpoint_id)?;

    // Copy to userspace
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, buf_len) };
    let copy_len = core::cmp::min(msg.len(), buf_len);
    buf[..copy_len].copy_from_slice(&msg[..copy_len]);

    Ok(copy_len)
}

/// sys_call(endpoint_token, msg_ptr, msg_len, reply_buf, reply_len, 0) -> Result<usize, Error>
pub fn sys_call(args: SyscallArgs) -> SyscallResult {
    // Send + Receive in one atomic operation
    sys_send(SyscallArgs::new(args.arg1, args.arg2, args.arg3, 0, 0, 0))?;
    sys_recv(SyscallArgs::new(args.arg1, args.arg4, args.arg5, 0, 0, 0))
}

/// sys_reply(msg_ptr, msg_len, 0, 0, 0, 0) -> Result<(), Error>
pub fn sys_reply(args: SyscallArgs) -> SyscallResult {
    let msg_ptr = args.arg1 as *const u8;
    let msg_len = args.arg2;

    // Validate user pointer
    userptr::validate_read(msg_ptr, msg_len)?;

    // Get current thread's reply endpoint
    let reply_endpoint = current_thread().reply_endpoint
        .ok_or(Error::NoReplyEndpoint)?;

    // Copy message from userspace
    let msg = unsafe { core::slice::from_raw_parts(msg_ptr, msg_len) };

    // Send reply
    IPC_MANAGER.reply(reply_endpoint, msg)?;

    Ok(0)
}
```

### Yield Handler (`kernel/src/syscall/handlers/yield.rs`)

```rust
/// sys_yield(0, 0, 0, 0, 0, 0) -> Ok(())
pub fn sys_yield(_args: SyscallArgs) -> SyscallResult {
    // Give up CPU to scheduler
    SCHEDULER.yield_current_thread();
    Ok(0)
}
```

---

## Userspace Syscall Interface (libcluu)

### Raw Syscall Wrappers (`libcluu/src/syscall/raw.rs`)

```rust
//! Raw syscall interface using inline assembly

use core::arch::asm;

/// Syscall numbers (must match kernel)
#[repr(usize)]
pub enum SyscallNumber {
    Send = 0,
    Recv = 1,
    Call = 2,
    Reply = 3,
    Yield = 4,
    Invoke = 5,
    DebugPrint = 255,
}

/// Raw syscall with 0 arguments
#[inline]
pub unsafe fn syscall0(number: SyscallNumber) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        in("rax") number as usize,
        lateout("rax") ret,
        out("rcx") _,  // Clobbered by SYSCALL
        out("r11") _,  // Clobbered by SYSCALL
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw syscall with 1 argument
#[inline]
pub unsafe fn syscall1(number: SyscallNumber, arg1: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        in("rax") number as usize,
        in("rdi") arg1,
        lateout("rax") ret,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw syscall with 2 arguments
#[inline]
pub unsafe fn syscall2(number: SyscallNumber, arg1: usize, arg2: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        in("rax") number as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        lateout("rax") ret,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw syscall with 6 arguments (maximum)
#[inline]
pub unsafe fn syscall6(
    number: SyscallNumber,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        in("rax") number as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,  // Note: R10 instead of RCX
        in("r8") arg5,
        in("r9") arg6,
        lateout("rax") ret,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}
```

### High-Level Syscall API (`libcluu/src/syscall/mod.rs`)

```rust
//! High-level syscall interface for userspace

mod raw;
pub mod ipc;
pub mod invoke;
pub mod types;

pub use types::*;

use crate::error::{Error, Result};
use raw::*;

/// Convert syscall return value to Result
#[inline]
fn syscall_result(ret: isize) -> Result<usize> {
    if ret >= 0 {
        Ok(ret as usize)
    } else {
        Err(Error::from_errno(-ret as i32))
    }
}

/// Yield CPU to scheduler
#[inline]
pub fn yield_now() -> Result<()> {
    unsafe {
        syscall_result(syscall0(SyscallNumber::Yield))?;
        Ok(())
    }
}

/// Print debug message (only in debug builds)
#[cfg(debug_assertions)]
pub fn debug_print(msg: &str) -> Result<()> {
    unsafe {
        syscall_result(syscall2(
            SyscallNumber::DebugPrint,
            msg.as_ptr() as usize,
            msg.len(),
        ))?;
        Ok(())
    }
}
```

### IPC Syscall Helpers (`libcluu/src/syscall/ipc.rs`)

```rust
use super::*;

/// Send IPC message to endpoint
pub fn send(endpoint: TokenHandle, msg: &[u8]) -> Result<()> {
    unsafe {
        syscall_result(syscall6(
            SyscallNumber::Send,
            endpoint.as_raw(),
            msg.as_ptr() as usize,
            msg.len(),
            0,
            0,
            0,
        ))?;
        Ok(())
    }
}

/// Receive IPC message from endpoint
pub fn recv(endpoint: TokenHandle, buf: &mut [u8]) -> Result<usize> {
    unsafe {
        syscall_result(syscall6(
            SyscallNumber::Recv,
            endpoint.as_raw(),
            buf.as_mut_ptr() as usize,
            buf.len(),
            0,
            0,
            0,
        ))
    }
}

/// Call (send + receive) to endpoint
pub fn call(endpoint: TokenHandle, msg: &[u8], reply_buf: &mut [u8]) -> Result<usize> {
    unsafe {
        syscall_result(syscall6(
            SyscallNumber::Call,
            endpoint.as_raw(),
            msg.as_ptr() as usize,
            msg.len(),
            reply_buf.as_mut_ptr() as usize,
            reply_buf.len(),
            0,
        ))
    }
}

/// Reply to IPC sender
pub fn reply(msg: &[u8]) -> Result<()> {
    unsafe {
        syscall_result(syscall6(
            SyscallNumber::Reply,
            msg.as_ptr() as usize,
            msg.len(),
            0,
            0,
            0,
            0,
        ))?;
        Ok(())
    }
}
```

### Invoke Syscall Helpers (`libcluu/src/syscall/invoke.rs`)

```rust
use super::*;

/// Invoke operations (must match kernel InvokeOp)
#[repr(usize)]
pub enum InvokeOp {
    ThreadCreate = 0,
    ThreadDestroy = 1,
    ThreadSuspend = 2,
    ThreadResume = 3,
    ThreadSetPriority = 4,
    SpaceCreate = 10,
    SpaceDestroy = 11,
    SpaceMap = 12,
    SpaceUnmap = 13,
    SpaceGrant = 14,
    TokenDerive = 20,
    TokenRevoke = 21,
    IrqAttach = 30,
    IrqAck = 31,
}

/// Generic invoke wrapper
#[inline]
pub fn invoke(
    token: TokenHandle,
    op: InvokeOp,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> Result<usize> {
    unsafe {
        syscall_result(syscall6(
            SyscallNumber::Invoke,
            token.as_raw(),
            op as usize,
            arg1,
            arg2,
            arg3,
            arg4,
        ))
    }
}

/// Create new address space
pub fn space_create(root_token: TokenHandle) -> Result<TokenHandle> {
    let handle = invoke(root_token, InvokeOp::SpaceCreate, 0, 0, 0, 0)?;
    Ok(TokenHandle::from_raw(handle))
}

/// Map page into address space
pub fn space_map(
    space_token: TokenHandle,
    virt_addr: usize,
    phys_addr: usize,
    flags: PageFlags,
) -> Result<()> {
    invoke(
        space_token,
        InvokeOp::SpaceMap,
        virt_addr,
        phys_addr,
        flags.bits(),
        0,
    )?;
    Ok(())
}

/// Create thread in address space
pub fn thread_create(
    space_token: TokenHandle,
    entry: usize,
    stack: usize,
    priority: u8,
) -> Result<TokenHandle> {
    let handle = invoke(
        space_token,
        InvokeOp::ThreadCreate,
        entry,
        stack,
        priority as usize,
        0,
    )?;
    Ok(TokenHandle::from_raw(handle))
}

/// Derive token with reduced rights
pub fn token_derive(
    parent_token: TokenHandle,
    rights: Rights,
    expire_at: u64,
    target_thread: usize,
) -> Result<TokenHandle> {
    let handle = invoke(
        parent_token,
        InvokeOp::TokenDerive,
        rights.bits() as usize,
        expire_at as usize,
        target_thread,
        0,
    )?;
    Ok(TokenHandle::from_raw(handle))
}
```

### Shared Types (`libcluu/src/syscall/types.rs`)

```rust
/// Token handle (opaque to userspace)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenHandle(usize);

impl TokenHandle {
    pub const fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    pub const fn as_raw(self) -> usize {
        self.0
    }
}

/// Rights bitmask (must match kernel)
bitflags! {
    pub struct Rights: u32 {
        const READ     = 1 << 0;
        const WRITE    = 1 << 1;
        const EXECUTE  = 1 << 2;
        const CREATE   = 1 << 3;
        const DESTROY  = 1 << 4;
        const GRANT    = 1 << 5;
        const MAP      = 1 << 6;
        const MANAGE   = 1 << 7;

        const THREAD_CONTROL = 1 << 8;
        const THREAD_SUSPEND = 1 << 9;

        const SPACE_MAP    = 1 << 16;
        const SPACE_UNMAP  = 1 << 17;
        const SPACE_GRANT  = 1 << 18;

        const IPC_SEND     = 1 << 24;
        const IPC_RECV     = 1 << 25;
        const IPC_CALL     = 1 << 26;

        const IRQ_HANDLE   = 1 << 28;
        const IRQ_ACK      = 1 << 29;
    }
}

/// Page mapping flags (must match kernel)
bitflags! {
    pub struct PageFlags: usize {
        const PRESENT   = 1 << 0;
        const WRITABLE  = 1 << 1;
        const USER      = 1 << 2;
        const EXECUTABLE = 1 << 3;
    }
}
```

---

## Security Properties

### 1. Unforgeable

Only kernel can create valid tokens (has secret key).

### 2. Non-enumerable

Opaque scopes prevent object discovery attacks.

### 3. Time-bounded

Mandatory expiration limits damage from stolen tokens.

### 4. Least Privilege

Rights masks enforce minimum necessary permissions.

### 5. Explicit Delegation

Issuer field tracks delegation chains.

### 6. Tamper-proof

Signature prevents modification of any field.

---

## Comparison with Other Systems

### vs. seL4 Capabilities

| Feature | seL4 | CLUU Tokens |
|---------|------|-------------|
| Unforgeable | ✓ | ✓ |
| Time-bounded | ✗ | ✓ (mandatory expiration) |
| Delegation | ✓ (badge) | ✓ (issuer field) |
| Revocation | Complex | Automatic (expiration) |
| Serializable | ✗ | ✓ (signature) |

### vs. L4 Capabilities

| Feature | L4 | CLUU Tokens |
|---------|-----|-------------|
| Unforgeable | ✓ | ✓ |
| Opaque IDs | ✓ | ✓ |
| Rights | ✓ | ✓ (bitmask) |
| Expiration | ✗ | ✓ |
| Signature | ✗ | ✓ |

### vs. JWT Tokens

| Feature | JWT | CLUU Tokens |
|---------|-----|-------------|
| Signature | ✓ | ✓ (HMAC) |
| Expiration | Optional | Mandatory |
| Scope | String | Opaque ID |
| Issuer | String | Enum (Kernel/Authority) |
| Kernel-enforced | ✗ | ✓ |

---

## Success Criteria

- [ ] Syscall count reduced from 14 to 7
- [ ] All operations go through sys_invoke()
- [ ] Token signature verification working
- [ ] Opaque scopes prevent enumeration
- [ ] Expiration checked on every use
- [ ] Rights enforcement prevents privilege escalation
- [ ] Derived tokens work (delegation)
- [ ] IPC can pass tokens
- [ ] Unit tests pass
- [ ] Integration tests pass
- [ ] Security tests pass (forgery, expiration, rights)

---

## Timeline

**Phase 8b Total**: ~2-3 weeks

- Phase 1 (Token Infrastructure): 3-4 days
- Phase 2 (Syscall Refactor): 3-4 days
- Phase 3 (Update Existing Code): 4-5 days
- Phase 4 (IPC Integration): 2-3 days
- Phase 5 (Testing): 2-3 days

---

## Notes

- This is a major refactor touching most of the kernel
- Requires careful testing to avoid security regressions
- Consider implementing behind feature flag initially
- Plan migration path for existing code
- Document security properties clearly
