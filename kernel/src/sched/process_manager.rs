// Retired in Phase 3 of the frame-typing + unified-process-model spec
// (2026-05-18-frame-typing-and-unified-process-model.md).
//
// ProcessManager and the global PROCESS_MANAGER static are removed.
// Procmgr (userspace) maintains the authoritative process table.
// Kernel thread lifecycle is managed by THREAD_MANAGER in thread_manager.rs.
