// Retired in Phase 3 of the frame-typing + unified-process-model spec
// (2026-05-18-frame-typing-and-unified-process-model.md).
//
// spawn_elf_process and spawn_kernel_process were helpers that wrapped
// ProcessManager to create Process objects.  The boot path (bootstrap.rs)
// never used these functions — it constructs Thread objects directly.
// These wrappers are removed along with the Process model they depended on.
