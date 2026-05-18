// Retired in Phase 3 of the frame-typing + unified-process-model spec
// (2026-05-18-frame-typing-and-unified-process-model.md).
//
// The kernel no longer maintains a Process struct.  Procmgr (userspace) is the
// sole owner of process lifecycle.  The kernel knows only: Threads, Address
// Spaces, Endpoints, Tokens, and Typed Frames.
//
// All types previously exported from this module (Process, ProcessId,
// ProcessState, ProcessType, ProcessInitState) are removed.  The boot path
// creates Thread objects directly; it never needed a Process wrapper.
