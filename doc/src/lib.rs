//! # CLUU — A Capability-Native Microkernel
//!
//! CLUU is a hobby microkernel and minimal POSIX-flavored userspace
//! written in Rust. It is seL4-inspired, pre-v1, and built around a
//! single design principle: **authority is structural, not
//! conventional**. The kernel knows three things — threads, capability
//! tokens, and IPC. Everything else lives in userspace.
//!
//! ## What these docs cover
//!
//! - [Getting Started](getting_started/index.html) — build, boot,
//!   login, and a tour of what works.
//! - [Design Philosophy](philosophy/index.html) — the capability
//!   model, why there is no runtime ACL, and the encapsulation
//!   invariants that shape every subsystem.
//! - [System Architecture](architecture/index.html) — the
//!   kernel/userspace split, service topology, and IPC flow.
//! - [The Kernel](kernel/index.html) — subsystem-by-subsystem deep
//!   dive: scheduler, memory management, IPC, capability tokens,
//!   syscalls, interrupts.
//! - [Memory Model](memory_model/index.html) — address space layout,
//!   frame typing, demand paging, COW fork, ASLR.
//! - [Capability Tokens](capability_tokens/index.html) — the
//!   HMAC-signed authority primitive and the `InvokeOp` dispatch table.
//! - [IPC](ipc/index.html) — pipes, shared memory, the invoke dispatch
//!   table.
//! - [Process Management & Sessions](procmgr/index.html) —
//!   root-procmgr, session-procmgr, and the hierarchical cap-derivation
//!   model.
//! - [Process Model](process_model/index.html) — threads vs processes,
//!   spawn lifecycle, exit cookies.
//! - [Container Encapsulation](containers/index.html) — why a CLUU
//!   "container" is not a Docker image, and how authority is declared
//!   at spawn time.
//! - [Session Encapsulation](sessions/index.html) — per-login
//!   isolation, the root-session godmode, and why cross-session
//!   visibility is a privilege.
//! - [Virtual Filesystem](vfs/index.html) — mount table, per-session
//!   views, monotone-narrowing view derivation.
//! - [Terminal Stack](terminal/index.html) — kbd, tty, console,
//!   vtmgr, compositor, cluuterm.
//! - [Storage Stack](storage/index.html) — ext2, virtio-blk,
//!   virtio-core.
//! - [Boot Flow](boot/index.html) — firmware → kernel → init →
//!   service spawn → login.
//! - [Service Catalog](services/index.html) — every userspace
//!   service, its IPC labels, and its role.
//! - [Roadmap](roadmap/index.html) — what works, what doesn't, what's
//!   next.
//! - [Audit](audit/index.html) — internal kernel audit findings and
//!   freeze scope.
//! - [Debugging](debugging/index.html) — GDB setup, serial markers,
//!   harness debugging.
//! - [Testing](testing/index.html) — Python harness, smoke tests,
//!   probe binaries.
//! - [Interpreter Porting](interpreter_porting/index.html) — porting
//!   MicroPython and other interpreters to CLUU.
//! - [Gotchas](gotchas/index.html) — structural traps discovered during
//!   implementation.

// Chapter modules — each includes its markdown source for rustdoc.
#[doc = include_str!("../book/getting_started.md")]
pub mod getting_started {}

#[doc = include_str!("../book/philosophy.md")]
pub mod philosophy {}

#[doc = include_str!("../book/architecture.md")]
pub mod architecture {}

#[doc = include_str!("../book/kernel.md")]
pub mod kernel {}

#[doc = include_str!("../book/capability_tokens.md")]
pub mod capability_tokens {}

#[doc = include_str!("../book/procmgr.md")]
pub mod procmgr {}

#[doc = include_str!("../book/containers.md")]
pub mod containers {}

#[doc = include_str!("../book/sessions.md")]
pub mod sessions {}

#[doc = include_str!("../book/vfs.md")]
pub mod vfs {}

#[doc = include_str!("../book/terminal.md")]
pub mod terminal {}

#[doc = include_str!("../book/storage.md")]
pub mod storage {}

#[doc = include_str!("../book/boot.md")]
pub mod boot {}

#[doc = include_str!("../book/services.md")]
pub mod services {}

#[doc = include_str!("../book/memory_model.md")]
pub mod memory_model {}

#[doc = include_str!("../book/ipc.md")]
pub mod ipc {}

#[doc = include_str!("../book/process_model.md")]
pub mod process_model {}

#[doc = include_str!("../book/roadmap.md")]
pub mod roadmap {}

#[doc = include_str!("../book/audit.md")]
pub mod audit {}

#[doc = include_str!("../book/debugging.md")]
pub mod debugging {}

#[doc = include_str!("../book/testing.md")]
pub mod testing {}

#[doc = include_str!("../book/interpreter_porting.md")]
pub mod interpreter_porting {}

#[doc = include_str!("../book/gotchas.md")]
pub mod gotchas {}
