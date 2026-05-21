//! IPC label constants for procmgr dispatch.
//!
//! Labels already defined in `cluu_wire` sub-modules are re-exported here
//! rather than duplicated, so there is a single source of truth for their
//! numeric values.  New labels that belong exclusively to root-procmgr or
//! session-procmgr are defined below.

// === legacy root-procmgr labels (from userspace/root-procmgr/src/main.rs) ===
pub const PROCMGR_EXIT_LABEL: u32 = 1;
pub const PROCMGR_SPAWN_LABEL: u32 = 2;
pub const PROCMGR_KILL_LABEL: u32 = 3;
pub const PROCMGR_FAULT_LABEL: u32 = 0xFA017;

// Re-exports from cluu_wire (single source of truth — do NOT duplicate values here).
// PROCMGR_SESSION_CREATE_LABEL = 89, PROCMGR_SESSION_DESTROY_LABEL = 90 live in
// cluu_wire::session; callers should use these re-exports, not the raw crate path.
pub use cluu_wire::spawn::PROCMGR_SPAWN_UNIFIED_LABEL;
pub use cluu_wire::primordial::PROCMGR_PRIMORDIAL_SEED_LABEL;
pub use cluu_wire::session::{
    PROCMGR_SESSION_CREATE_LABEL,
    PROCMGR_SESSION_DESTROY_LABEL,
    PROCMGR_SESSION_QUERY_LABEL,
    PROCMGR_SESSION_SUBSCRIBE_LABEL,
    PROCMGR_SESSION_DERIVE_TOKEN_LABEL,
    SESSION_ENDED_LABEL,
    PROCMGR_SESSION_SET_LEADER_LABEL,
    COMPOSITOR_SESSION_HANDOFF_LABEL,
};

// === new (root-procmgr only) ===
pub const PROCMGR_SERVICE_SPAWN_LABEL: u32 = 0xA002;
pub const PROCMGR_PROC_QUERY_ALL_LABEL: u32 = 0xA003;
pub const PROCMGR_ESCALATE_LABEL: u32 = 0xA004;
pub const PROCMGR_SHUTDOWN_LABEL: u32 = 0xA005;

// === new (session-procmgr only) ===
pub const SESSION_PROCMGR_SPAWN_LABEL: u32 = 0xB000;
pub const SESSION_PROCMGR_KILL_LABEL: u32 = 0xB001;
pub const SESSION_PROCMGR_WAIT_LABEL: u32 = 0xB002;
pub const SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL: u32 = 0xB003;
pub const SESSION_PROCMGR_PIPE_CREATE_LABEL: u32 = 0xB004;
pub const SESSION_PROCMGR_PIPE_CLOSE_LABEL: u32 = 0xB005;
pub const SESSION_PROCMGR_PG_CREATE_LABEL: u32 = 0xB006;
pub const SESSION_PROCMGR_PG_ATTACH_LABEL: u32 = 0xB007;
pub const SESSION_PROCMGR_PG_SIGNAL_LABEL: u32 = 0xB008;
pub const SESSION_PROCMGR_CTTY_QUERY_LABEL: u32 = 0xB009;
