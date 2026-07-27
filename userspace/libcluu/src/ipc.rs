//! IPC helpers
//!
//! Higher-level IPC wrappers using the Message type.

use crate::boot::process_info;
use crate::error::Result;
use crate::syscall;
use crate::types::*;
use crate::Error;
use alloc::vec::Vec;
use core::cmp;
use core::mem::{align_of, size_of};
use core::sync::atomic::{fence, Ordering};

pub const PROC_EXIT_LABEL: u32 = 1;
pub const CONSOLE_WRITE_LABEL: u32 = 1;
pub const CONSOLE_CLEAR_LABEL: u32 = 2;
pub const CONSOLE_CURSOR_LABEL: u32 = 3;
pub const CONSOLE_BLINK_LABEL: u32 = 4;
pub const CONSOLE_WRITE_SYNC_LABEL: u32 = 5;
pub const IPC_CHUNK_BYTES_DEFAULT: usize = 256;
pub const IPC_SEND_RETRIES_DEFAULT: u32 = 256;
pub const IPC_BACKOFF_MAX_DEFAULT: u32 = 64;
/// Inline stack buffer size for small IPC payloads (avoid heap use).
const IPC_INLINE_STACK_MAX: usize = 512;
pub const KBD_EVENT_LABEL: u32 = 1;
pub const KBD_RAW_LABEL: u32 = 0x600;
pub const TTY_READ_LABEL: u32 = 1;
pub const TTY_WRITE_LABEL: u32 = 2;
pub const TTY_CTL_LABEL: u32 = 3;
pub const TTY_REGISTER_LABEL: u32 = 4;
pub const TTY_WRITE_SYNC_LABEL: u32 = 5;
pub const TTY_READ_REQUEST_LABEL: u32 = 6;
pub const CONSOLE_FB_INFO_LABEL: u32 = 6;
pub const CONSOLE_CREDIT_REFILL_LABEL: u32 = 8;
pub const TTY_POLL_QUERY_LABEL: u32 = 7;
/// Pipe data message: write end sends payload with this label.
#[cfg(feature = "posix")]
pub use crate::posix::pipe::PIPE_DATA_LABEL;
/// Pipe EOF marker: write end sends 0-byte message with this label to signal EOF.
#[cfg(feature = "posix")]
pub use crate::posix::pipe::PIPE_EOF_LABEL;
/// Set the foreground process-group for a session (procmgr/shell → tty).
/// words[0] = session_id, words[1] = pgid.
pub const TTY_SET_FG_LABEL: u32 = 40;
/// Query the foreground process-group for a session (any → tty).
/// words[0] = session_id. Reply: words[0]=0, words[1]=pgid (0 if none).
pub const TTY_GET_FG_LABEL: u32 = 41;
pub const CONSOLE_ACTIVATE_LABEL: u32 = 8;
pub const CONSOLE_DEACTIVATE_LABEL: u32 = 9;
pub const PROCMGR_QUERY_CTTY_LABEL: u32 = 11;
/// service → vtmgr: pin a named service to a specific VT slot.
/// words[0] = VT index to pin.  Payload = service name (UTF-8, no NUL).
/// Sent once at startup by the service that owns a non-text VT (e.g. compositor → 4).
/// vtmgr uses this to route switch requests to the right backend without relying
/// on magic numbers in its own source.
pub const VTMGR_PIN_VT_LABEL: u32 = 16;
/// vtmgr → console: create a new VT buffer.
/// words[0] = VT index (0-3).
pub const CONSOLE_CREATE_VT_LABEL: u32 = 17;
/// vtmgr → console: atomic VT switch (single message replaces deactivate+activate).
/// words[0] = old VT index, words[1] = new VT index.
pub const CONSOLE_SWITCH_VT_LABEL: u32 = 21;
/// per-VT write: tty → console with VT index routing.
/// words[0] = payload length, words[1] = VT index.
pub const CONSOLE_WRITE_VT_LABEL: u32 = 18;
/// per-VT synchronous write (call): tty → console with VT index routing.
/// words[0] = payload length, words[1] = VT index.
pub const CONSOLE_WRITE_VT_SYNC_LABEL: u32 = 19;
/// kbd → console: scroll a VT's viewport for scrollback navigation.
/// words[0] = VT index, words[1] = direction (0=back/up, 1=forward/down).
pub const CONSOLE_SCROLL_VT_LABEL: u32 = 25;
/// Generic service spawn request (any service → procmgr).
/// words[0] = payload length, words[1] = priority,
/// words[2] = TOKEN_EXTRA_0 mode (0=none, 1=listen, 2=grantable),
/// words[3] = param override count.
/// Payload = path\0 + param overrides (each: u16 index LE + u64 value LE = 10 bytes).
pub const PROCMGR_SPAWN_SERVICE_LABEL: u32 = 20;

// ──────────────────────────────────────────────────────────────────────
// Driver spawn param slot indices (D3.2).
//
// These are indices into ProcessInfo.params[] used by drivermgr to pass
// device-specific data to a spawned driver via PROCMGR_SPAWN_SERVICE_LABEL
// param overrides. Slots 19-29 are in the "reserved for future use" range
// (slots 0-18 are already assigned — see boot.rs). The procmgr spawn
// handler rejects reserved metadata slots 10-15 but allows 19+.
//
// PARAM_DEVICE_PATH: for PCI devices, packs BDF as (bus<<16|dev<<8|func);
//   drivers reconstruct the canonical /pci/XX:YY.Z path from it in D3.5.
//   ACPI device spawn is D6.
// PARAM_PCI_BDF: same packed BDF as PARAM_DEVICE_PATH (for PCI devices).
// PARAM_PCI_BAR0..5: the 6 PCI base address register values.
// PARAM_IRQ_LINE: the IRQ line assigned to the device.
// PARAM_DMA_BASE / PARAM_DMA_PAGES: DMA region (D3.5 allocates; D3.2
//   passes 0 as placeholder when rule.dma is true).
// ──────────────────────────────────────────────────────────────────────
pub const PARAM_DEVICE_PATH: usize = 19;
pub const PARAM_PCI_BDF: usize = 20;
pub const PARAM_PCI_BAR0: usize = 21;
pub const PARAM_PCI_BAR1: usize = 22;
pub const PARAM_PCI_BAR2: usize = 23;
pub const PARAM_PCI_BAR3: usize = 24;
pub const PARAM_PCI_BAR4: usize = 25;
pub const PARAM_PCI_BAR5: usize = 26;
pub const PARAM_IRQ_LINE: usize = 27;
pub const PARAM_DMA_BASE: usize = 28;
pub const PARAM_DMA_PAGES: usize = 29;

/// Token request entry: slot index + rights bitmask.
/// Sent in the spawn payload after param overrides so procmgr can
/// derive capability tokens from its authority and place them in
/// the spawned driver's ProcessInfo token array.
///
/// Wire format: 2 bytes slot (LE u16) + 4 bytes rights (LE u32) = 6 bytes.
/// procmgr derives from self.token (boot root) with the requested rights.

/// Set the per-client VFS view (mount list). Request from procmgr to VFS.
///
/// Message words:
///   [0] payload length in bytes
///   [1] target client_tid (0 = sender_tid)
///   [2] mount count
///   [3] CapProfile bits (0 = clear profile on empty mount update)
///   [4] container_id (u64 fits in usize on x86_64)
///
/// Per-mount wire layout:
///   u16 src_len LE | u16 dst_len LE | u8 flags | u64 memfs_cid LE |
///   src_bytes (src_len) | dst_bytes (dst_len)
///
/// Flags bit 0 = writable. `memfs_cid = 0` resolves the mount against the
/// global MountTable; `memfs_cid > 0` resolves against that container's
/// per-container MemFs backend (procmgr owns the keying).
pub const VFS_SET_VIEW_LABEL: u32 = 21;

/// Procmgr → VFS: clean up container storage on process exit or destroy.
/// words[1] = container_id (u64 as usize),
/// words[2] = mode (0 = exit: delete tmp/ contents; 1 = destroy: delete entire c-{id}/ tree).
pub const VFS_CONTAINER_CLEANUP_LABEL: u32 = 22;

/// Container run: payload = image name (UTF-8), optionally followed by an
/// FdInherit blob, ARGV trailer, REDIR trailer, ENV trailer, and/or CWD trailer.
/// Wire order (when all trailers present):
///   `[name][FdInherit][ARGV trailer][REDIR trailer][ENV trailer][CWD trailer]`
/// Procmgr strips trailers in reverse order: CWD → ENV → REDIR → ARGV.
/// Reply: words[0] = errno, words[1] = pid, words[2] = container_id.
pub const PROCMGR_CONTAINER_RUN_LABEL: u32 = 24;

/// Magic marker for the CWD trailer at the end of a spawn payload.
/// Bytes in little-endian order: 'C','W','D',' ' = 0x43, 0x57, 0x44, 0x20.
///
/// Trailer layout (always at the very end of the payload, after any FdInherit blob):
///   `[cwd_bytes][u32 cwd_len LE][u32 CWD_MAGIC LE]`
///
/// The trailer is optional; payloads without it are treated as "no parent cwd"
/// and the child's cwd defaults to `/`. Procmgr strips the trailer before
/// computing fd_inherit_offset, so callers must append the trailer **after** the
/// FdInherit blob.
pub const CWD_MAGIC: u32 = 0x2044_5743;

/// Magic sentinel for the ARGV trailer (ASCII "ARGV" little-endian).
/// Present only when argc > 0 in `build_container_run_payload_with_argv`
/// output; procmgr strips it in `split_argv_trailer` after `split_cwd_trailer`.
pub const ARGV_MAGIC: u32 = 0x5647_5241;

/// Container list: no payload.
/// Reply: payload = "name pid container_id\n" lines.
pub const PROCMGR_CONTAINER_LIST_LABEL: u32 = 25;

// PROCMGR_SESSION_LOGIN_LABEL removed — replaced by cluu_wire::session::PROCMGR_SESSION_CREATE_LABEL
// (Task 9, Plan 3: session lifecycle refactor)
/// Session death: procmgr → tty (send). words[0]=vt_instance.
pub const PROCMGR_SESSION_DEATH_LABEL: u32 = 31;
/// Privilege escalation (sudo): shell → procmgr (call).
/// words[0]=payload_len, words[1]=notify_endpoint.
/// Payload: password\0command\0.
/// Reply: words[0]=errno, words[1]=pid, words[2]=exit_cookie,
///        words[3]=child_stdin_send, words[4]=container_id.
pub const PROCMGR_ESCALATE_LABEL: u32 = 32;
/// Identity switch (su): shell → procmgr (call).
/// words[0]=payload_len, words[1]=notify_endpoint.
/// Payload: username\0password\0.
/// Reply: words[0]=errno, words[1]=pid, words[2]=exit_cookie,
///        words[3]=child_stdin_send, words[4]=container_id.
pub const PROCMGR_SU_LABEL: u32 = 33;
/// Container stats query: shell → procmgr (call).
/// Reply: words[0]=status, words[1]=record_count, words[2]=total_containers,
///        words[3]=total_sessions. Payload = 64-byte fixed records.
pub const PROCMGR_CONTAINER_STATS_LABEL: u32 = 35;
/// Shutdown request. words[0]: 0=poweroff, 1=reboot.
pub const PROCMGR_SHUTDOWN_LABEL: u32 = 36;
/// Proc query: VFS → procmgr (call).
/// words[0] = query_type (0=status, 1=stat, 2=cmdline, 3=list),
/// words[1] = target_pid (0=self), words[2] = original_caller_tid.
/// Reply: words[0]=errno, words[1]=data_len or pid_count, payload=content.
pub const PROCMGR_PROC_QUERY_LABEL: u32 = 37;

/// List all PIDs in a session: caller → session-procmgr (async via ipc_send).
/// words[4]=reply_ep, words[5]=caller_cookie.
/// Handler replies directly via `ipc_send(reply_ep, ...)`: words[0]=errno,
/// words[1]=pid_count, words[5]=caller_cookie, payload=raw LE u32 PIDs.
pub const PROCMGR_LIST_PIDS_LABEL: u32 = 0x4A;

/// Per-PID info: caller → session-procmgr (async via ipc_send).
/// words[0]=pid, words[4]=reply_ep, words[5]=caller_cookie.
/// Handler replies directly via `ipc_send(reply_ep, ...)`: words[0]=errno,
/// words[5]=caller_cookie, payload=postcard(ProcInfo) on hit.
pub const PROCMGR_PROC_INFO_LABEL: u32 = 0x4B;

/// drivermgr → procmgr: register a notify endpoint for exit notifications.
/// words[0] = pid
/// words[1] = notify_endpoint
/// Reply: words[0] = 0 on success, errno on failure
///
/// Used by drivermgr after PROCMGR_SPAWN_SERVICE_LABEL to direct
/// PROC_EXIT_LABEL notifications for the spawned driver to drivermon's
/// notify endpoint. procmgr looks up the exit_cookie via pid_to_cookie
/// and inserts (cookie, notify_endpoint) into the exit_notify map.
pub const PROCMGR_REGISTER_EXIT_NOTIFY_LABEL: u32 = 0x52;

/// drivermgr → procmgr: set the fault handler endpoint for a spawned
/// driver thread. words[0] = pid, words[1] = fault_endpoint_token.
/// Reply: words[0] = 0 on success, errno on failure.
///
/// procmgr looks up the thread_token via pid_to_cookie → exit_table
/// (cookie → thread_token) and calls `thread_set_fault_endpoint`.
/// Used by drivermgr after PROCMGR_SPAWN_SERVICE_LABEL so driver
/// faults are delivered to drivermon's fault endpoint instead of
/// killing the thread outright.
pub const PROCMGR_SET_FAULT_EP_LABEL: u32 = 0x53;

/// shell/rc.boot → procmgr: spawn a container by image name (like autostart).
/// words[0] = payload_len (set by send_msg_with_payload)
/// Payload: image_name (UTF-8, no NUL) — e.g. "console", "compositor"
/// Reply: words[0] = 0 on success, errno on failure. words[1] = pid.
pub const PROCMGR_START_IMAGE_LABEL: u32 = 0x54;

/// Allocate a new pipe. Reply: words[0]=status, words[1]=write_token,
/// words[2]=read_token, words[3]=pipe_id.
pub const PROCMGR_PIPE_CREATE_LABEL: u32 = 38;

/// Release the caller's tokens for a pipe. words[0]=pipe_id. Idempotent.
pub const PROCMGR_PIPE_CLOSE_LABEL: u32 = 39;

// Process-group / job-control IPC labels (Phase 4 Plan D).
/// Create a new process group. Reply: words[0]=status, words[1]=pgid.
pub const PROCMGR_PG_CREATE_LABEL: u32 = 80;
/// Attach a pid to a pgid. words[0]=pgid, words[1]=pid. Fire-and-forget.
pub const PROCMGR_PG_ATTACH_LABEL: u32 = 81;
/// Deliver a signal to all members of a pgid.
/// words[0]=pgid, words[1]=signum. Fire-and-forget.
pub const PROCMGR_PG_SIGNAL_LABEL: u32 = 82;
/// Suspend all threads of every pid in a pgid.
/// words[0]=pgid. Fire-and-forget.
pub const PROCMGR_PG_SUSPEND_LABEL: u32 = 83;
/// Resume all threads of every pid in a pgid.
/// words[0]=pgid. Fire-and-forget.
pub const PROCMGR_PG_RESUME_LABEL: u32 = 84;
/// Query the pgid of the process that owns a given tid.
/// words[0]=tid. Reply: words[0]=status, words[1]=pgid (0 if not in any group).
pub const PROCMGR_PID_PGID_QUERY_LABEL: u32 = 85;
/// Async notification from procmgr to parent on job state change.
/// words[0]=pgid, words[1]=pid, words[2]=state (1=Stopped,2=Continued),
/// words[3]=exit_code.
pub const PROCMGR_JOB_NOTIFY_LABEL: u32 = 86;

// ──────────────────────────────────────────────────────────────────────
// Compositor protocol (sub-project A — see
// doc/book/terminal.md §7)
// ──────────────────────────────────────────────────────────────────────

/// App → compositor:client. Request a new window. Payload: title bytes.
/// Wire layout: words[0]=payload_len (title), words[1]=req_w, words[2]=req_h,
/// words[3]=app_input_endpoint, words[4]=flags (see COMP_WIN_FLAG_*).
pub const COMP_WIN_REGISTER_LABEL: u32 = 90;
/// Compositor → app reply.
pub const COMP_WIN_REGISTER_REPLY: u32 = 91;

/// WIN_REGISTER flag: no compositor chrome (border/title); status bar row 0
/// suppressed while this window is focused.  Compositor sizes the window to
/// the full cell grid regardless of req_w/req_h.
pub const COMP_WIN_FLAG_FULLSCREEN: u32 = 1 << 0;
/// WIN_REGISTER flag: suppress chrome (border/title). Window keeps requested dims.
pub const COMP_WIN_FLAG_NO_CHROME: u32 = 1 << 1;
/// WIN_REGISTER flag: modal. Pinned to z-top, grabs input, Esc dismisses.
pub const COMP_WIN_FLAG_MODAL: u32 = 1 << 2;
/// App → compositor:client. Mark a damage rect on a registered window.
pub const COMP_WIN_DAMAGE_LABEL: u32 = 92;
/// App → compositor:client. Free a window.
pub const COMP_WIN_DESTROY_LABEL: u32 = 93;
/// App → compositor:client. Re-render chrome with a new title.
pub const COMP_WIN_SET_TITLE_LABEL: u32 = 94;
/// kbd → compositor:input. A raw key event while the compositor's VT is active.
pub const COMP_KBD_EVENT_LABEL: u32 = 95;
/// compositor → app input endpoint. Forwarded keystroke or close-request.
pub const COMP_INPUT_FORWARD_LABEL: u32 = 96;
/// vt mgr / init → compositor:control. Take fb ownership and repaint.
pub const COMP_VT_ACTIVATE_LABEL: u32 = 97;
/// vt mgr / init → compositor:control. Pause drawing; retain state.
pub const COMP_VT_DEACTIVATE_LABEL: u32 = 98;
/// init → compositor:control. Free all windows and exit cleanly.
pub const COMP_SHUTDOWN_LABEL: u32 = 99;
/// compositor → app input endpoint. "You may present a new frame now."
/// Pacing signal: app blocks until this arrives, then renders one frame.
pub const COMP_FRAME_READY_LABEL: u32 = 100;
/// compositor → app input endpoint. User or compositor requested window close.
/// App should unregister its PTS, destroy the window, and exit cleanly.
pub const COMP_CLOSE_REQUEST_LABEL: u32 = 101;
/// App → compositor:client. Query screen dimensions in cells.
/// Reply: words[0]=cols, words[1]=rows.
pub const COMP_WIN_QUERY_SCREEN_LABEL: u32 = 102;
// COMPOSITOR_READY_LABEL removed — compositor no longer swaps system/user mode
// (Task 9, Plan 3: session lifecycle refactor)
/// compositor → app input endpoint. Window was resized to new pixel dimensions.
/// words[0] = pixel_width (u32), words[1] = pixel_height (u32).
pub const COMP_WIN_CONFIGURE_LABEL: u32 = 103;
/// app → compositor: request window resize to new cell dimensions.
/// words[0] = window_id, words[1] = cols, words[2] = rows.
/// Compositor clamps to screen bounds and replies with WIN_CONFIGURE.
pub const COMP_WIN_RESIZE_LABEL: u32 = 105;
/// mouse → vtmgr:input → compositor:input. Mouse movement/button event.
/// words[0]=dx(i32 as usize), words[1]=dy(i32 as usize), words[2]=buttons(u8: L=1,R=2,M=4).
pub const MOUSE_EVENT_LABEL: u32 = 104;

/// App → compositor:client. Declare a pixel sub-region within a text window.
///
/// After this call, the compositor stops glyph-blitting cells inside the
/// region and instead reads raw ARGB32 pixels from a separate SHM frame
/// token, blitting them directly to the backbuffer at framebuffer resolution.
///
/// Wire layout:
///   words[0] = window_id
///   words[1] = cell_x (region left edge in window-local cell coords)
///   words[2] = cell_y (region top edge in window-local cell coords)
///   words[3] = cell_w (region width in cells)
///   words[4] = cell_h (region height in cells)
///   words[5] = pixel_frame_token (frame token for the pixel SHM buffer)
///
/// The pixel SHM buffer must contain
///   `cell_w * GLYPH_W * cell_h * GLYPH_H * 4` bytes of ARGB32 data
/// (row-major, stride = `cell_w * GLYPH_W`), where GLYPH_W=8, GLYPH_H=16.
///
/// Sending this message again with the same window_id replaces the prior
/// region. Sending with cell_w=0 and cell_h=0 clears the pixel region.
pub const COMP_WIN_SET_PIXEL_REGION_LABEL: u32 = 106;

// --- Input routing (vtmgr today; inputd post-extraction). ---
// client → vtmgr: request a VT switch. vtmgr decides per policy.
// Words: [vt: u32]. Reply: words[0] = errno (0 ok).
pub const VTMGR_REQUEST_VT_SWITCH_LABEL: u32 = 110;
// compositor → vtmgr: take/release modal lock on VT switching.
// Reserved per login-flow §4.6; impl is stub today.
pub const VTMGR_LOCK_VT_SWITCH_LABEL:   u32 = 111;
pub const VTMGR_UNLOCK_VT_SWITCH_LABEL: u32 = 112;

// ──────────────────────────────────────────────────────────────────────
// PTS (pseudo-terminal slave) protocol.
//
// PTS_REGISTER_LABEL   — cluuterm → VFS: allocate a new /dev/pts/<id>.
//     words[0] = notify_endpoint (usize): VFS sends PTS_CLOSED_LABEL here
//                when the last fd on the pts is closed.
//     Reply: words[0] = errno (0 = ok), words[1] = id (u32).
//
// PTS_UNREGISTER_LABEL — cluuterm → VFS: explicitly release a pts id.
//     words[0] = id (u32).
//     Only the original registrant (matched by sender_tid) may unregister.
//     Reply: words[0] = errno.
//
// PTS_READ_LABEL  — client → VFS → forwarded to owner: read from pts.
//     words[0] = id (u32), words[1] = len.
//     Reply forwarded back.
//
// PTS_WRITE_LABEL — client → VFS → forwarded to owner: write to pts.
//     words[0] = id (u32).  Payload = bytes.
//     Reply forwarded back.
//
// PTS_IOCTL_LABEL — client → VFS → forwarded to owner: ioctl on pts.
//     words[0] = id (u32), words[1] = request code.
//     Reply forwarded back.
//
// PTS_POLL_LABEL  — client → VFS → forwarded to owner: poll readiness.
//     words[0] = id (u32).
//     Reply forwarded back.
//
// PTS_CLOSED_LABEL — VFS → owner (notify_endpoint): last fd closed.
//     words[0] = id (u32).  Fire-and-forget (no reply expected).
// ──────────────────────────────────────────────────────────────────────
pub const PTS_REGISTER_LABEL: u32   = 0x70;
pub const VFS_REGISTER_DEV_LABEL: u32 = 0x71;
pub const DEV_READ_REQUEST_LABEL: u32 = 0x72;
pub const PTS_UNREGISTER_LABEL: u32 = 0x71;
pub const PTS_READ_LABEL: u32       = 0x72;
pub const PTS_WRITE_LABEL: u32      = 0x73;
pub const PTS_IOCTL_LABEL: u32      = 0x74;
pub const PTS_POLL_LABEL: u32       = 0x75;
pub const PTS_CLOSED_LABEL: u32     = 0x76;

// ──────────────────────────────────────────────────────────────────────
// VFS_DERIVE_CHILD_FD_LABEL — procmgr → VFS: clone an open file to a
// new client_id and mint a narrowed VFS token for the child.
//
// Sent by procmgr's FdInherit handler when spawning a child that inherits a
// VFS-backed fd (pts, ext2, memfs).  VFS mints the derived token from
// its own full-rights endpoint (`self.endpoint`), which it holds
// legitimately via `endpoint_create` at boot.
//
// Request words:
//     words[0] = parent_client_id  — caller's (parent process) client_id in VFS
//     words[1] = parent_remote_fd  — VFS-side fd number for the parent's open file
//     words[2] = child_rights      — rights bits to narrow to (e.g. READ|WRITE)
//     words[3] = child_tid         — new thread id; used as new client_id for child
//
// Reply words:
//     words[0] = status            — 0 on success; errno cast to usize on error
//     words[1] = derived_handle    — narrowed VFS token scoped to vfs:main
//     words[2] = child_client_id   — echo of child_tid passed in
//     words[3] = child_remote_fd   — freshly allocated fd slot under child_client_id
// ──────────────────────────────────────────────────────────────────────
pub const VFS_DERIVE_CHILD_FD_LABEL: u32 = 0x77;

// VFS_DERIVE_CHILD_FD_BATCH_LABEL — procmgr → VFS: batch-clone multiple
// open files to a child in one IPC round-trip. Equivalent to N
// VFS_DERIVE_CHILD_FD_LABEL calls but saves N-1 IPC dispatch cycles
// (VFS is single-threaded, so each IPC is serial).
//
// Request:
//     words[0] = payload_len (overwritten by send_msg_with_payload)
//     words[1] = count       — number of entries (max 4)
//     words[2] = parent_tid  — parent's tid (for logging)
//     payload  = count × 32 bytes, each entry:
//                [u64 parent_cid, u64 parent_fd, u64 rights, u64 child_tid]
//
// Reply (via reply_to_sender_with_payload):
//     words[0] = payload_len
//     words[1] = overall_status (0 = all OK, errno of first failure)
//     words[2] = child_cid (same for all entries = child_tid)
//     payload  = count × 32 bytes, each entry:
//                [u64 status, u64 derived_token, u64 child_cid, u64 child_rfd]
pub const VFS_DERIVE_CHILD_FD_BATCH_LABEL: u32 = 0x78;

/// VFS_MOUNT_LABEL — service → VFS: mount my endpoint at a path.
/// words[0] = endpoint_token (the service's listen endpoint).
/// payload = "mount_path\0service_name\0"
/// Reply: words[0] = 0 on success, errno on failure.
pub const VFS_MOUNT_LABEL: u32 = 0x79;

// virtio-blk raw-block session IPC labels (Phase 6 of virtio-blk modern redesign).
// `BLK_OPEN_SESSION` and `BLK_CLOSE_SESSION` go to the driver's listen endpoint.
// `BLK_SUBMIT` is fire-and-forget into the driver. `BLK_COMPLETE` and
// `BLK_SUBMIT_NACK` flow back to the caller's per-session completion endpoint.
pub const BLK_OPEN_SESSION: u32 = 0x310;
pub const BLK_SUBMIT: u32 = 0x311;
pub const BLK_COMPLETE: u32 = 0x312;
pub const BLK_CLOSE_SESSION: u32 = 0x313;
pub const BLK_SUBMIT_NACK: u32 = 0x314;
/// Procmgr → virtio-blk: a tid has exited; reap any sessions owned by it.
/// words[0] = exited tid. No reply.
pub const BLK_TID_CLEANUP: u32 = 0x315;

// ──────────────────────────────────────────────────────────────────────
// virtio-snd audio IPC labels.
//
// `AUDIO_OPEN_SESSION` and `AUDIO_CLOSE` go to the driver's listen endpoint.
// `AUDIO_SUBMIT_PCM` is fire-and-forget; `AUDIO_COMPLETE` flows back to the
// caller's per-session completion endpoint.
// words[0] = session_id (or completion_endpoint for OPEN_SESSION).
pub const AUDIO_OPEN_SESSION: u32 = 0x600;
pub const AUDIO_SUBMIT_PCM: u32 = 0x601;
pub const AUDIO_COMPLETE: u32 = 0x602;
pub const AUDIO_CLOSE: u32 = 0x603;
/// Procmgr → virtio-snd: a tid has exited; reap any sessions owned by it.
pub const AUDIO_TID_CLEANUP: u32 = 0x604;
/// Client → virtio-snd: query supported format/rate/channel bitmasks.
/// Reply: words[0]=status(0=ok), words[1]=formats, words[2]=rates, words[3]=channels.
pub const AUDIO_QUERY_CAPS: u32 = 0x605;

// ──────────────────────────────────────────────────────────────────────
// devmgr IPC labels — block device manager service.
//
// devmgr owns block devices and mints BlockRegion capability tokens for
// sessions.  Three labels cover the full lifecycle:
//
//   DEVMGR_REGISTER_LABEL   — driver → devmgr at boot (device registration)
//   DEVMGR_GRANT_REGION_LABEL — procmgr → devmgr at session creation
//   DEVMGR_REVOKE_LABEL     — procmgr → devmgr at session exit
//
// See `userspace/devmgr/src/main.rs` for the wire format of each label.
// ──────────────────────────────────────────────────────────────────────
pub const DEVMGR_REGISTER_LABEL: u32 = 0x500;
pub const DEVMGR_GRANT_REGION_LABEL: u32 = 0x501;
pub const DEVMGR_REVOKE_LABEL: u32 = 0x502;
/// drivermgr → devmgr: mint a scoped IRQ_HANDLE token for a specific IRQ line.
/// words[1] = irq_line (u8). Reply: words[0]=status, words[1]=token_handle.
pub const DEVMGR_MINT_IRQ_CAP_LABEL: u32 = 0x503;
pub const DEVMGR_REGISTER_CHAR_LABEL: u32 = 0x510;
pub const DEVMGR_GRANT_DEVICE_LABEL: u32 = 0x511;
pub const DEVMGR_LIST_FOR_ENVELOPE_LABEL: u32 = 0x512;

// drivermgr IPC labels — VFS /proc/devices backend queries drivermgr for the
// device tree.  QUERY_DEVICES returns the full tree as text (one line per
// device); QUERY_DEVICE takes a canonical path (e.g. "/pci/00:04.0") as the
// IPC payload and returns the per-device detail text.  Both reply via
// `reply_to_sender_with_payload`.  Numbers chosen in the 0x520 range to
// leave room for further devmgr labels in 0x503..0x50F.
pub const DRIVERMGR_QUERY_DEVICES_LABEL: u32 = 0x520;
pub const DRIVERMGR_QUERY_DEVICE_LABEL: u32 = 0x521;

/// drivermon → drivermgr: request respawn of a driver for a device.
/// words[0] = payload_len (set by send_msg_with_payload)
/// words[1] = driver_image_len (0 = use current image from bind rule)
/// Payload: "device_path\0driver_image" (driver_image optional if len=0)
///
/// Fire-and-forget: drivermon sends this on exit-notify when restart
/// policy allows a restart. drivermgr looks up the BindRule by
/// device_path, respawns via spawn_driver, and the new REGISTER IPC
/// arrives at drivermon with the new PID.
pub const DRIVERMGR_RESPAWN_DEVICE_LABEL: u32 = 0x523;

/// drivermon → drivermgr: notify device state change.
/// words[0] = payload_len (set by send_msg_with_payload)
/// words[1] = state (0=Bound, 1=Restarting, 2=Failed)
/// Payload: device_path (UTF-8, no NUL)
///
/// Fire-and-forget: drivermon sends this when a supervised driver
/// transitions state. drivermgr updates the DeviceNode.state in the
/// device tree so `/proc/devices` reflects the current lifecycle.
pub const DRIVERMGR_DEVICE_STATE_LABEL: u32 = 0x524;

/// shell/rc.boot → drivermgr: request bus re-scan + driver spawn.
/// words[0] = payload_len (set by send_msg_with_payload)
/// Payload: bus name (UTF-8, no NUL) — "pci" or "acpi"
///
/// Synchronous reply: words[0] = 0 on success, errno on failure.
pub const DRIVERMGR_PROBE_LABEL: u32 = 0x525;

/// Kernel → fault handler: fault IPC message label.
///
/// Sent by the kernel to a thread's fault endpoint when the thread
/// faults (page fault, GP, invalid opcode, div-by-zero). Wire format:
///   words[0] = fault_type (0=PageFault, 1=GP, 2=InvalidOpcode, 3=DivByZero)
///   words[1] = fault_addr
///   words[2] = error_code
///   words[3] = rip
///   words[4] = thread_id (tid)
///   words[5] = reply_id (0 if thread already dead, non-zero if waiting)
///
/// Reply convention (via `ipc_reply` with reply_id): label == 0 →
/// RESUME the thread (restore saved context); label != 0 → KILL the
/// thread. If reply_id is 0, no reply is needed — the thread is
/// already dead.
pub const FAULT_LABEL: u32 = 0xFA017;

// drivermon IPC labels — drivermgr → drivermon supervision channel.
//
// REGISTER  — drivermgr → drivermon after spawning a driver: adds a
//             RuntimeEntry to the supervised runtime table.
// RESPAWN   — drivermgr → drivermon after re-spawning a crashed driver:
//             increments restart_count for the device's entry.
// REBIND    — drivermgr → drivermon when a fallback driver replaces the
//             primary: replaces driver_image, returns the old PID.
//
// Wire format (all three): payload-only message. `words[0]` is the
// payload length (set by `parse_message`); message-specific data lives
// in `words[1..]` and the payload is a NUL-separated string list.
//
// REGISTER words: [payload_len, pid, policy, has_fallback]
//   payload:      "device_path\0driver_image[\0fallback]"
//   reply words:  [status, 0, 0, 0, 0, 0]  (status 0 = OK)
// RESPAWN words:  [payload_len]
//   payload:      "device_path"
//   reply words:  [status, restart_count, 0, 0, 0, 0]
//   status 0 = OK, 1 = NotFound
// REBIND words:   [payload_len]
//   payload:      "device_path\0new_driver_image"
//   reply words:  [status, old_pid, 0, 0, 0, 0]
//   status 0 = OK, 1 = NotFound (old_pid 0 when not found)
//
// Numbers chosen in the 0x530 range — above drivermgr query labels
// (0x520..0x521) and below audio labels (0x600+).
pub const DRIVERMON_REGISTER_LABEL: u32 = 0x530;
pub const DRIVERMON_RESPAWN_LABEL: u32 = 0x531;
pub const DRIVERMON_REBIND_LABEL: u32 = 0x532;

// tpmd IPC labels (per-service label space)
pub const TPMD_STARTUP_LABEL: u32    = 1;
pub const TPMD_PCR_READ_LABEL: u32   = 2;
pub const TPMD_PCR_EXTEND_LABEL: u32 = 3;
pub const TPMD_GET_INFO_LABEL: u32   = 4;
pub const TPMD_CREATE_PRIMARY_LABEL: u32 = 5;
pub const TPMD_SEAL_LABEL: u32       = 6;
pub const TPMD_UNSEAL_LABEL: u32     = 7;
pub const TPMD_CREATE_AIK_LABEL: u32 = 8;
pub const TPMD_QUOTE_LABEL: u32      = 9;

pub const TTY_FG_FLAG_FORWARD_CTRL_C: usize = 1 << 0;
pub const TTY_FG_FLAG_NOTIFY_CTRL_C: usize = 1 << 1;
pub const TTY_CTL_SYNC: u32 = 1;
pub const CALL_COOKIE_TAG: u8 = 1;
pub const CALL_COOKIE_WORD: usize = 5;
pub const SHARED_RING_MAGIC: u32 = 0x434c_5555; // "CLUU"
pub const SHARED_RING_VERSION: u16 = 1;
pub const SHARED_RING_DEFAULT_MAP_FLAGS: usize = 0x03; // readable + writable
pub const SHARED_RING_DEFAULT_GRANT_FLAGS: usize = 0x02; // writable
const SHARED_RING_MIN_CAPACITY: usize = 2;

/// Tag indicating the message contains a reply ID
pub const REPLY_ID_TAG: u8 = 2;
/// Word index where reply ID is stored
pub const REPLY_ID_WORD: usize = 5;

/// Tag indicating this is an async call (sent via `ipc_send`, not `ipc_call`).
/// `words[4]` = reply endpoint token, `words[5]` = correlation cookie.
/// Servers should reply via `ipc_send(words[4], ...)` echoing `words[5]`.
pub const ASYNC_REPLY_TAG: u8 = 3;
pub const ASYNC_REPLY_EP_WORD: usize = 4;
pub const ASYNC_REPLY_COOKIE_WORD: usize = 5;

// ── netd IPC labels ───────────────────────────────────────────────────────
pub const NET_SOCKET: u32        = 0x700;
pub const NET_BIND: u32          = 0x701;
pub const NET_CONNECT: u32       = 0x702;
pub const NET_LISTEN: u32        = 0x703;
pub const NET_ACCEPT: u32        = 0x704;
pub const NET_SEND: u32          = 0x705;
pub const NET_RECV: u32          = 0x706;
pub const NET_CLOSE: u32         = 0x707;
pub const NET_POLL: u32          = 0x708;
pub const NET_DNS_RESOLVE: u32   = 0x709;
pub const NET_GET_MAC: u32       = 0x70A;
pub const NET_PKT_SEND: u32      = 0x70B;
pub const NET_PKT_RECV: u32      = 0x70D;
pub const NET_REGISTER_RECV: u32 = 0x70C;

pub const NET_SOCK_TCP: usize   = 1;
pub const NET_SOCK_UDP: usize   = 2;
pub const NET_SOCK_ICMP: usize  = 3;
pub const NET_SOCK_RAW: usize   = 4;

/// Shared-ring metadata stored in the first bytes of the shared region.
///
/// This is a single-producer/single-consumer ring with one reserved slot to
/// distinguish full/empty states.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SharedRingHeader {
    pub magic: u32,
    pub version: u16,
    pub reserved: u16,
    pub capacity: u32,
    pub read_idx: u32,
    pub write_idx: u32,
    pub notify_seq: u32,
    pub reserved2: u32,
}

impl SharedRingHeader {
    pub const fn bytes() -> usize {
        size_of::<SharedRingHeader>()
    }
}

/// Virtual region backing a shared ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedRingRegion {
    pub base: usize,
    pub bytes: usize,
    pub pages: usize,
}

/// Single-producer/single-consumer shared ring view over a mapped region.
pub struct SharedRing<'a> {
    header: &'a mut SharedRingHeader,
    data: &'a mut [u8],
    capacity: usize,
}

impl<'a> SharedRing<'a> {
    /// Return total bytes required for a ring with `capacity` payload bytes.
    pub fn bytes_for_capacity(capacity: usize) -> Result<usize> {
        if capacity < SHARED_RING_MIN_CAPACITY {
            return Err(Error::InvalidArgument);
        }
        SharedRingHeader::bytes()
            .checked_add(capacity)
            .ok_or(Error::Overflow)
    }

    /// Initialize a shared ring in `backing`.
    pub fn initialize(backing: &'a mut [u8]) -> Result<Self> {
        let (header, data) = split_shared_ring_backing(backing)?;
        if data.len() < SHARED_RING_MIN_CAPACITY {
            return Err(Error::BufferTooSmall);
        }
        let capacity = data.len().min(u32::MAX as usize);
        *header = SharedRingHeader {
            magic: SHARED_RING_MAGIC,
            version: SHARED_RING_VERSION,
            reserved: 0,
            capacity: capacity as u32,
            read_idx: 0,
            write_idx: 0,
            notify_seq: 0,
            reserved2: 0,
        };
        Ok(Self {
            header,
            data: &mut data[..capacity],
            capacity,
        })
    }

    /// Attach to an already initialized shared ring.
    pub fn attach(backing: &'a mut [u8]) -> Result<Self> {
        let (header, data) = split_shared_ring_backing(backing)?;
        if header.magic != SHARED_RING_MAGIC || header.version != SHARED_RING_VERSION {
            return Err(Error::InvalidState);
        }
        let capacity = header.capacity as usize;
        if capacity < SHARED_RING_MIN_CAPACITY || capacity > data.len() {
            return Err(Error::InvalidState);
        }
        Ok(Self {
            header,
            data: &mut data[..capacity],
            capacity,
        })
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        let read = self.read_idx();
        let write = self.write_idx();
        if write >= read {
            write - read
        } else {
            self.capacity - read + write
        }
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.capacity
            .saturating_sub(self.available_read())
            .saturating_sub(1)
    }

    /// Push as many bytes as possible from `src`. Returns bytes written.
    pub fn push(&mut self, src: &[u8]) -> usize {
        if src.is_empty() {
            return 0;
        }
        let to_write = cmp::min(src.len(), self.available_write());
        if to_write == 0 {
            return 0;
        }
        let write = self.write_idx();
        let first = cmp::min(to_write, self.capacity - write);
        self.data[write..write + first].copy_from_slice(&src[..first]);
        if to_write > first {
            let second = to_write - first;
            self.data[..second].copy_from_slice(&src[first..first + second]);
        }
        fence(Ordering::Release);
        self.set_write_idx((write + to_write) % self.capacity);
        to_write
    }

    /// Pop as many bytes as possible into `dst`. Returns bytes read.
    pub fn pop(&mut self, dst: &mut [u8]) -> usize {
        if dst.is_empty() {
            return 0;
        }
        let to_read = cmp::min(dst.len(), self.available_read());
        if to_read == 0 {
            return 0;
        }
        let read = self.read_idx();
        let first = cmp::min(to_read, self.capacity - read);
        dst[..first].copy_from_slice(&self.data[read..read + first]);
        if to_read > first {
            let second = to_read - first;
            dst[first..first + second].copy_from_slice(&self.data[..second]);
        }
        fence(Ordering::Release);
        self.set_read_idx((read + to_read) % self.capacity);
        to_read
    }

    /// Reset read/write cursors.
    pub fn reset(&mut self) {
        self.set_read_idx(0);
        self.set_write_idx(0);
    }

    /// Increment notify sequence to signal peer after data movement.
    pub fn bump_notify_seq(&mut self) -> u32 {
        let next = self.notify_seq().wrapping_add(1);
        unsafe {
            core::ptr::write_volatile(&mut self.header.notify_seq, next);
        }
        next
    }

    #[inline]
    pub fn notify_seq(&self) -> u32 {
        unsafe { core::ptr::read_volatile(&self.header.notify_seq) }
    }

    #[inline]
    fn read_idx(&self) -> usize {
        let raw = unsafe { core::ptr::read_volatile(&self.header.read_idx) } as usize;
        if self.capacity == 0 {
            0
        } else {
            raw % self.capacity
        }
    }

    #[inline]
    fn write_idx(&self) -> usize {
        let raw = unsafe { core::ptr::read_volatile(&self.header.write_idx) } as usize;
        if self.capacity == 0 {
            0
        } else {
            raw % self.capacity
        }
    }

    #[inline]
    fn set_read_idx(&mut self, idx: usize) {
        unsafe {
            core::ptr::write_volatile(&mut self.header.read_idx, idx as u32);
        }
    }

    #[inline]
    fn set_write_idx(&mut self, idx: usize) {
        unsafe {
            core::ptr::write_volatile(&mut self.header.write_idx, idx as u32);
        }
    }
}

fn split_shared_ring_backing(backing: &mut [u8]) -> Result<(&mut SharedRingHeader, &mut [u8])> {
    if backing.len() < SharedRingHeader::bytes() {
        return Err(Error::BufferTooSmall);
    }
    let align = align_of::<SharedRingHeader>();
    if !(backing.as_ptr() as usize).is_multiple_of(align) {
        return Err(Error::InvalidArgument);
    }
    let (header_bytes, data) = backing.split_at_mut(SharedRingHeader::bytes());
    let header = unsafe { &mut *(header_bytes.as_mut_ptr() as *mut SharedRingHeader) };
    Ok((header, data))
}

/// Allocate and map a local shared-ring region in the caller address space.
pub fn alloc_shared_ring_region(
    space_token: usize,
    capacity: usize,
    map_flags: usize,
) -> Result<SharedRingRegion> {
    let min_bytes = SharedRing::bytes_for_capacity(capacity)?;
    let pages = crate::mem::pages_for_size(min_bytes);
    let bytes = pages
        .checked_mul(crate::mem::PAGE_SIZE)
        .ok_or(Error::Overflow)?;
    let base = {
        let mut vspace = crate::vspace::VSPACE.lock();
        vspace.alloc(bytes)?
    };
    match crate::syscall::space_map_range(space_token, base, 0, map_flags, pages, 0) {
        Ok(_) => Ok(SharedRingRegion { base, bytes, pages }),
        Err(err) => {
            let mut vspace = crate::vspace::VSPACE.lock();
            let _ = vspace.free(base, bytes);
            Err(err)
        }
    }
}

/// Unmap and release a previously allocated shared-ring region.
pub fn free_shared_ring_region(space_token: usize, region: SharedRingRegion) -> Result<()> {
    crate::syscall::space_unmap(space_token, region.base, region.pages)?;
    let mut vspace = crate::vspace::VSPACE.lock();
    vspace.free(region.base, region.bytes)?;
    Ok(())
}

/// Grant a mapped ring region to another address space page-by-page.
///
/// Returns the number of pages granted.
pub fn grant_shared_ring_region(
    source_space_token: usize,
    target_space_token: usize,
    source_base: usize,
    target_base: usize,
    bytes: usize,
    grant_flags: usize,
) -> Result<usize> {
    if bytes == 0 {
        return Err(Error::InvalidArgument);
    }
    if !source_base.is_multiple_of(crate::mem::PAGE_SIZE)
        || !target_base.is_multiple_of(crate::mem::PAGE_SIZE)
    {
        return Err(Error::InvalidArgument);
    }
    let pages = crate::mem::pages_for_size(bytes);
    for page_idx in 0..pages {
        let src = source_base + page_idx * crate::mem::PAGE_SIZE;
        let dst = target_base + page_idx * crate::mem::PAGE_SIZE;
        crate::syscall::space_grant(
            source_space_token,
            target_space_token,
            src,
            dst,
            grant_flags,
        )?;
    }
    Ok(pages)
}

/// Send a message (one-way)
pub fn send(endpoint_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    // Convert Message to bytes and call syscall::ipc_send
    let msg_bytes = msg.as_bytes();
    syscall::ipc_send(endpoint_token, msg_bytes)
}

/// Send a message with an inline payload appended after the Message header.
pub fn send_with_payload(endpoint_token: usize, label: u32, payload: &[u8]) -> Result<()> {
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = payload.len();
    send_msg_with_payload(endpoint_token, &msg, payload)
}

/// Send a caller-constructed Message with an inline payload.
///
/// `words[0]` is always overwritten with `payload.len()` to enforce the
/// protocol invariant that receivers rely on for `parse_message`.
pub fn send_msg_with_payload(endpoint_token: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let mut msg = msg.clone();
    msg.words[0] = payload.len();
    let header = msg.as_bytes();
    let total_len = header.len() + payload.len();
    if total_len <= IPC_INLINE_STACK_MAX {
        let mut buffer = [0u8; IPC_INLINE_STACK_MAX];
        buffer[..header.len()].copy_from_slice(header);
        buffer[header.len()..total_len].copy_from_slice(payload);
        return syscall::ipc_send(endpoint_token, &buffer[..total_len]);
    }

    let mut buffer = Vec::with_capacity(total_len);
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    syscall::ipc_send(endpoint_token, &buffer)
}

/// Send a message with an inline payload, retrying on busy endpoints.
pub fn send_with_retry(endpoint_token: usize, label: u32, payload: &[u8]) -> Result<()> {
    send_with_retry_timeout(endpoint_token, label, payload, 0)
}

/// Send a message with an inline payload, retrying on busy endpoints with backoff.
///
/// When `max_retries` is 0, retries indefinitely.
///
/// Note: Error::WouldBlock means the kernel blocked the thread and will wake it
/// when space is available. We just retry - the kernel handles the blocking.
pub fn send_with_retry_timeout(
    endpoint_token: usize,
    label: u32,
    payload: &[u8],
    max_retries: u32,
) -> Result<()> {
    let max_backoff = IPC_BACKOFF_MAX_DEFAULT;
    let mut backoff = 1u32;
    let mut retries = 0u32;
    loop {
        match send_with_payload(endpoint_token, label, payload) {
            Ok(()) => return Ok(()),
            Err(Error::WouldBlock) => {
                // Kernel blocked us and will wake when space is available
                // Just retry - no need for backoff since kernel handles blocking
                continue;
            }
            Err(Error::Busy) => {
                retries = retries.saturating_add(1);
                if max_retries != 0 && retries > max_retries {
                    return Err(Error::Busy);
                }
                for _ in 0..backoff {
                    let _ = syscall::yield_cpu();
                }
                backoff = (backoff.saturating_mul(2)).min(max_backoff);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Synchronous write to TTY - waits for acknowledgement before returning.
/// Use this before exiting to ensure output is flushed.
pub fn tty_write_sync(endpoint_token: usize, payload: &[u8]) -> Result<()> {
    let mut msg = Message::new(TTY_WRITE_SYNC_LABEL, [0; 6], 1);
    msg.words[0] = payload.len();
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(endpoint_token, &msg, payload, &mut reply)
}

/// Call (send + wait for reply) with an inline payload appended after the Message header.
pub fn call_with_payload(
    endpoint_token: usize,
    msg: &Message,
    payload: &[u8],
    reply: &mut Message,
) -> Result<()> {
    let header = msg.as_bytes();
    let total_len = header.len() + payload.len();
    let reply_bytes = reply.as_bytes_mut();
    if total_len <= IPC_INLINE_STACK_MAX {
        let mut buffer = [0u8; IPC_INLINE_STACK_MAX];
        buffer[..header.len()].copy_from_slice(header);
        buffer[header.len()..total_len].copy_from_slice(payload);
        loop {
            match syscall::ipc_call(endpoint_token, &buffer[..total_len], reply_bytes) {
                Ok(_) => return Ok(()),
                Err(Error::WouldBlock) => { let _ = syscall::yield_cpu(); }
                Err(err) => return Err(err),
            }
        }
    }

    let mut buffer = Vec::with_capacity(total_len);
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    loop {
        match syscall::ipc_call(endpoint_token, &buffer, reply_bytes) {
            Ok(_) => return Ok(()),
            Err(Error::WouldBlock) => { let _ = syscall::yield_cpu(); }
            Err(err) => return Err(err),
        }
    }
}

pub fn call_with_payload_timeout(
    endpoint_token: usize,
    msg: &Message,
    payload: &[u8],
    reply: &mut Message,
    timeout_ms: usize,
) -> Result<()> {
    let header = msg.as_bytes();
    let total_len = header.len() + payload.len();
    let reply_bytes = reply.as_bytes_mut();
    if total_len <= IPC_INLINE_STACK_MAX {
        let mut buffer = [0u8; IPC_INLINE_STACK_MAX];
        buffer[..header.len()].copy_from_slice(header);
        buffer[header.len()..total_len].copy_from_slice(payload);
        loop {
            match syscall::ipc_call_timeout(endpoint_token, &buffer[..total_len], reply_bytes, timeout_ms) {
                Ok(_) => return Ok(()),
                Err(Error::Timeout) => return Err(Error::Timeout),
                Err(Error::WouldBlock) => { let _ = syscall::yield_cpu(); }
                Err(err) => return Err(err),
            }
        }
    }
    let mut buffer = Vec::with_capacity(total_len);
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    loop {
        match syscall::ipc_call_timeout(endpoint_token, &buffer, reply_bytes, timeout_ms) {
            Ok(_) => return Ok(()),
            Err(Error::Timeout) => return Err(Error::Timeout),
            Err(Error::WouldBlock) => { let _ = syscall::yield_cpu(); }
            Err(err) => return Err(err),
        }
    }
}

/// Receive a message
pub fn recv(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    let msg_bytes = msg.as_bytes_mut();
    loop {
        match syscall::ipc_recv(endpoint_token, msg_bytes) {
            Ok(_) => return Ok(()),
            Err(Error::WouldBlock) => {
                let _ = syscall::yield_cpu();
            }
            Err(err) => return Err(err),
        }
    }
}

/// Call (send + wait for reply)
pub fn call(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    let msg_copy = msg.clone();
    let send_bytes = msg_copy.as_bytes();
    let reply_bytes = msg.as_bytes_mut();
    loop {
        match syscall::ipc_call(endpoint_token, send_bytes, reply_bytes) {
            Ok(0) => { let _ = syscall::yield_cpu(); }
            Ok(_) => return Ok(()),
            Err(Error::WouldBlock) => { let _ = syscall::yield_cpu(); }
            Err(err) => return Err(err),
        }
    }
}

/// Synchronous call with timeout
///
/// Like `call`, but returns `Err(Error::Timeout)` if the server does not
/// reply within `timeout_ms` milliseconds. Retries on WouldBlock up to the
/// deadline (each retry re-enters the kernel which re-computes its own
/// internal deadline, so the overall wall-clock bound is approximate).
///
/// `timeout_ms` of 0 is equivalent to `call` (block forever).
pub fn call_with_timeout(
    endpoint_token: usize,
    msg: &mut Message,
    _flags: IpcFlags,
    timeout_ms: usize,
) -> Result<()> {
    let msg_copy = msg.clone();
    let send_bytes = msg_copy.as_bytes();
    let reply_bytes = msg.as_bytes_mut();
    loop {
        match syscall::ipc_call_timeout(endpoint_token, send_bytes, reply_bytes, timeout_ms) {
            Ok(_) => return Ok(()),
            Err(Error::WouldBlock) => { let _ = syscall::yield_cpu(); }
            Err(err) => return Err(err),
        }
    }
}

/// Extract reply ID from a received call message
///
/// Returns the reply ID if the message was from a call, None otherwise.
pub fn extract_reply_id(msg: &Message) -> Option<usize> {
    if msg.tag.extra == REPLY_ID_TAG {
        Some(msg.words[REPLY_ID_WORD])
    } else {
        None
    }
}

/// Reply to a received call message using the reply token
///
/// # Arguments
///
/// - `reply_token`: The reply token extracted from the received call message
/// - `msg`: Reply message to send
/// - `_flags`: IPC flags (currently unused)
pub fn reply(reply_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    let msg_bytes = msg.as_bytes();
    syscall::ipc_reply(reply_token, msg_bytes)?;
    Ok(())
}

/// Reply with an additional payload appended after the message header.
///
/// `words[0]` is always overwritten with `payload.len()` to enforce the
/// protocol invariant that receivers rely on for `parse_message`.
pub fn reply_with_payload(reply_token: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let mut msg = msg.clone();
    msg.words[0] = payload.len();
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    syscall::ipc_reply(reply_token, &buffer)?;
    Ok(())
}

/// Reply to a message sender, handling both synchronous (`ipc_reply`) and
/// asynchronous (`ipc_send` to reply endpoint) reply paths.
///
/// If the original message has `ASYNC_REPLY_TAG`, the reply is sent via
/// `ipc_send` to the reply endpoint in `words[4]`, with the correlation
/// cookie from `words[5]` echoed back. Otherwise, the reply is sent via
/// `ipc_reply` using the reply token extracted from the message.
///
/// Servers should use this when they need to handle both sync (`ipc_call`)
/// and async (`IpcCallFuture`) callers transparently.
pub fn reply_to_sender(
    original_msg: &Message,
    reply_msg: &Message,
    fallback_ep: usize,
    _flags: IpcFlags,
) -> Result<()> {
    if original_msg.tag.extra == ASYNC_REPLY_TAG {
        let reply_ep = original_msg.words[ASYNC_REPLY_EP_WORD];
        let cookie = original_msg.words[ASYNC_REPLY_COOKIE_WORD];
        let mut reply_with_cookie = reply_msg.clone();
        reply_with_cookie.words[ASYNC_REPLY_COOKIE_WORD] = cookie;
        send(reply_ep, &reply_with_cookie, IpcFlags::empty())
    } else {
        let reply_token = extract_reply_id(original_msg).unwrap_or(fallback_ep);
        reply(reply_token, reply_msg, IpcFlags::empty())
    }
}

/// Like `reply_to_sender` but appends a payload after the Message header.
/// Handles both sync (`ipc_reply`) and async (`ipc_send` to reply endpoint) paths.
pub fn reply_to_sender_with_payload(
    original_msg: &Message,
    reply_msg: &Message,
    payload: &[u8],
    fallback_ep: usize,
) -> Result<()> {
    if original_msg.tag.extra == ASYNC_REPLY_TAG {
        let reply_ep = original_msg.words[ASYNC_REPLY_EP_WORD];
        let cookie = original_msg.words[ASYNC_REPLY_COOKIE_WORD];
        let mut reply_with_cookie = reply_msg.clone();
        reply_with_cookie.words[0] = payload.len();
        reply_with_cookie.words[ASYNC_REPLY_COOKIE_WORD] = cookie;
        send_msg_with_payload(reply_ep, &reply_with_cookie, payload)
    } else {
        let reply_token = extract_reply_id(original_msg).unwrap_or(fallback_ep);
        reply_with_payload(reply_token, reply_msg, payload)
    }
}

/// Copy an IPC call cookie from a request into a reply.
///
/// Servers should call this when replying to ipc_call() so the kernel
/// can route the reply to the correct caller even if calls overlap.
pub fn copy_call_cookie(reply: &mut Message, request: &Message) {
    if request.tag.extra != CALL_COOKIE_TAG {
        return;
    }
    reply.tag.extra = CALL_COOKIE_TAG;
    reply.words[CALL_COOKIE_WORD] = request.words[CALL_COOKIE_WORD];
}

/// Reply and receive next message (server loop optimization)
///
/// Note: This is currently implemented as reply() + recv() separately.
/// In the future, this could be a single optimized syscall.
pub fn reply_recv(endpoint_token: usize, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    // Send reply first
    reply(endpoint_token, msg, flags)?;
    // Then receive next message
    recv(endpoint_token, msg, flags)
}

/// Call with payload, receiving both message and reply payload.
///
/// Returns (reply_message, bytes_in_reply_payload).
pub fn call_with_reply_buf(
    endpoint_token: usize,
    msg: &Message,
    send_payload: &[u8],
    reply_buf: &mut [u8],
) -> Result<(Message, usize)> {
    use core::mem::size_of;

    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + send_payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(send_payload);

    let bytes_received = syscall::ipc_call(endpoint_token, &buffer, reply_buf)?;

    if bytes_received < size_of::<Message>() {
        return Err(Error::InvalidState);
    }

    // Parse the reply message
    let reply_msg = unsafe { (reply_buf.as_ptr() as *const Message).read_unaligned() };
    let payload_len = bytes_received - size_of::<Message>();

    Ok((reply_msg, payload_len))
}

/// Notify the parent process manager that this process is exiting.
pub fn notify_exit(exit_code: i32) -> Result<()> {
    let info = process_info();
    if info.exit_token == 0 {
        // No parent to notify (e.g., init process)
        return Ok(());
    }

    let msg = Message::new(
        PROC_EXIT_LABEL,
        [info.exit_cookie, exit_code as usize, 0, 0, 0, 0],
        2,
    );
    send(info.exit_token, &msg, IpcFlags::empty())
}

/// Parse an IPC message buffer into a Message header + payload slice.
///
/// `words[0]` is read as the payload byte count. Malformed lengths are
/// clamped to zero (defensive — header is still returned for routing).
pub fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < core::mem::size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let header = core::mem::size_of::<Message>();
    let payload_len = msg.words[0];
    // Avoid `header + payload_len <= buf.len()` — that wraps on usize
    // and produces an inverted slice (56..44) that panics.
    let end = if payload_len <= buf.len().saturating_sub(header) {
        header + payload_len
    } else {
        header
    };
    Some((msg, &buf[header..end]))
}

/// Build a Message with words[0] = payload_len and extra data in words[1..].
pub fn make_payload_message(label: u32, payload_len: usize, extra_words: &[usize]) -> Message {
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = payload_len;
    let mut count: u8 = 1;
    for (idx, word) in extra_words.iter().enumerate() {
        if idx + 1 >= msg.words.len() {
            break;
        }
        msg.words[idx + 1] = *word;
        count += 1;
    }
    msg.tag.words = count;
    msg
}

/// Build a `PROCMGR_CONTAINER_RUN_LABEL` payload with optional argv.
///
/// Wire format:
///   `[name_bytes][argv[0]\0][argv[1]\0]...[u32 argv_bytes_len LE][u32 ARGV_MAGIC LE][cwd_trailer]`
///
/// `argc` is encoded in the ARGV trailer when argc > 0; empty-argv payloads
/// stay byte-for-byte compatible with pre-Task-5 output (no ARGV trailer).
///
/// Returns `(payload, argc)`. Procmgr strips the ARGV trailer in
/// `split_argv_trailer` after `split_cwd_trailer`.
#[cfg(feature = "posix")]
pub fn build_container_run_payload_with_argv(name: &str, args: &[&str]) -> (Vec<u8>, usize) {
    use crate::boot::CWD_MAX;

    let argc = args.len();
    let argv_bytes_est: usize = args.iter().map(|a| a.len() + 1).sum();
    let mut payload = Vec::with_capacity(name.len() + argv_bytes_est + CWD_MAX + 16);
    payload.extend_from_slice(name.as_bytes());

    if argc > 0 {
        let argv_start = payload.len();
        for arg in args {
            payload.extend_from_slice(arg.as_bytes());
            payload.push(0);
        }
        let argv_bytes_len = (payload.len() - argv_start) as u32;
        payload.extend_from_slice(&argv_bytes_len.to_le_bytes());
        payload.extend_from_slice(&ARGV_MAGIC.to_le_bytes());
    }

    let cwd_string = crate::posix::current_dir_string();
    let cwd_bytes = cwd_string.as_bytes();
    let cwd_len = cwd_bytes.len().min(CWD_MAX);
    payload.extend_from_slice(&cwd_bytes[..cwd_len]);
    payload.extend_from_slice(&(cwd_len as u32).to_le_bytes());
    payload.extend_from_slice(&CWD_MAGIC.to_le_bytes());

    (payload, argc)
}

/// Magic sentinel for the REDIR trailer (ASCII "REDI" little-endian).
pub const REDIR_MAGIC: u32 = 0x52454449;

/// Magic sentinel for the ENV trailer (ASCII "ENV " little-endian).
///
/// Trailer layout (sits between the REDIR and CWD trailers on the wire):
///   `[env_bytes][u32 env_bytes_len LE][u32 ENV_MAGIC LE]`
/// where `env_bytes` is "KEY=VALUE\0KEY=VALUE\0..." packed.
pub const ENV_MAGIC: u32 = 0x2056_4E45;

/// One entry in the FdInherit blob passed in a `CONTAINER_RUN` payload.
///
/// FdInherit = per-child-fd inheritance manifest passed at spawn time.
/// NOT access-control. Each entry tells procmgr which parent cap or VFS handle
/// to install at the named child fd. Monotone-decreasing: parent must already
/// hold what it lists.
///
/// Wire format per entry (32 bytes):
///   bytes  0– 3: target_fd (u32 LE)
///   bytes  4– 7: flags     (u32 LE; bit 0 = IS_PIPE)
///   bytes  8–15: endpoint  (usize LE; legacy path: pipes, tty)
///   bytes 16–23: vfs_client_id (usize LE; 0 = not VFS-backed)
///   bytes 24–31: vfs_remote_fd (usize LE; 0 = not VFS-backed)
pub struct FdInherit {
    pub target_fd:     u32,
    pub is_pipe:       bool,
    pub endpoint:      usize,
    /// VFS client ID for the source fd (0 for non-VFS fds such as pipes/tty).
    pub vfs_client_id: usize,
    /// VFS-side fd number for the source fd (0 for non-VFS fds).
    pub vfs_remote_fd: usize,
}

/// One file redirection entry packed into a `CONTAINER_RUN` payload.
pub struct RedirAction {
    /// Target fd to redirect (0=stdin, 1=stdout, 2=stderr).
    pub target_fd: u8,
    /// Open flags: 1=O_WRONLY|O_TRUNC, 2=O_WRONLY|O_APPEND, 3=O_RDONLY.
    pub flags: u8,
    /// Path to the file.
    pub path: alloc::string::String,
}

/// Build a `CONTAINER_RUN` payload that includes argv AND FdInherit entries.
///
/// Wire format (in order):
///   `[name_bytes]`
///   `[u32 FD_INHERIT_MAGIC LE][u32 count LE][(u32 fd, u32 flags, u64 ep, u64 vfs_client_id, u64 vfs_remote_fd) * count]`
///   `[argv[0]\0][argv[1]\0]...[u32 argv_bytes_len LE][u32 ARGV_MAGIC LE]`
///   `[cwd_bytes][u32 cwd_len LE][u32 CWD_MAGIC LE]`
///
/// FdInherit comes before the ARGV trailer so that procmgr's `split_argv_trailer`
/// (which looks at the last 4 bytes before the CWD) can cleanly strip the ARGV
/// block, leaving `[name][FdInherit]` in `effective_payload`.
///
/// Returns `(payload_bytes, argc, fd_inherit_offset)` where `fd_inherit_offset` is the
/// byte offset of the FdInherit blob measured from the start of the pre-CWD
/// payload view (i.e. from index 0).  Caller sets `msg.words[2] = fd_inherit_offset`.
///
/// If `fd_inherit` is empty, `fd_inherit_offset` is returned as `0` and no FdInherit blob is
/// written, matching the existing no-FdInherit wire format.
#[cfg(feature = "posix")]
pub fn build_container_run_payload_with_argv_and_fd_inherit(
    name: &str,
    args: &[&str],
    fd_inherit: &[FdInherit],
) -> (Vec<u8>, usize, usize) {
    use crate::boot::CWD_MAX;

    let argc = args.len();
    let argv_bytes_est: usize = args.iter().map(|a| a.len() + 1).sum();
    let mut payload =
        Vec::with_capacity(name.len() + argv_bytes_est + 32 * fd_inherit.len() + 16 + CWD_MAX + 24);
    payload.extend_from_slice(name.as_bytes());

    // FdInherit blob immediately after the image name — before ARGV trailer.
    // This ordering is required because procmgr's split_argv_trailer checks
    // the last 4 bytes of the effective_payload (post-CWD-strip) for
    // ARGV_MAGIC, so ARGV must be the last block before CWD.
    let fd_inherit_offset = if fd_inherit.is_empty() { 0 } else { payload.len() };
    if !fd_inherit.is_empty() {
        // Magic value 0x46444143 spells "FDAC" in ASCII — kept for historical
        // wire-format compatibility; the concept has since been renamed FdInherit.
        const FD_INHERIT_MAGIC: u32 = 0x46444143;
        payload.extend_from_slice(&FD_INHERIT_MAGIC.to_le_bytes());
        payload.extend_from_slice(&(fd_inherit.len() as u32).to_le_bytes());
        for entry in fd_inherit {
            // bytes 0–3: target_fd
            payload.extend_from_slice(&entry.target_fd.to_le_bytes());
            // bytes 4–7: flags
            let flags: u32 = if entry.is_pipe { 0x01 } else { 0 };
            payload.extend_from_slice(&flags.to_le_bytes());
            // bytes 8–15: endpoint
            payload.extend_from_slice(&(entry.endpoint as u64).to_le_bytes());
            // bytes 16–23: vfs_client_id
            payload.extend_from_slice(&(entry.vfs_client_id as u64).to_le_bytes());
            // bytes 24–31: vfs_remote_fd
            payload.extend_from_slice(&(entry.vfs_remote_fd as u64).to_le_bytes());
        }
    }

    // ARGV trailer comes after FdInherit, directly before CWD.
    if argc > 0 {
        let argv_start = payload.len();
        for arg in args {
            payload.extend_from_slice(arg.as_bytes());
            payload.push(0);
        }
        let argv_bytes_len = (payload.len() - argv_start) as u32;
        payload.extend_from_slice(&argv_bytes_len.to_le_bytes());
        payload.extend_from_slice(&ARGV_MAGIC.to_le_bytes());
    }

    // CWD trailer is always last.
    let cwd_string = crate::posix::current_dir_string();
    let cwd_bytes = cwd_string.as_bytes();
    let cwd_len = cwd_bytes.len().min(CWD_MAX);
    payload.extend_from_slice(&cwd_bytes[..cwd_len]);
    payload.extend_from_slice(&(cwd_len as u32).to_le_bytes());
    payload.extend_from_slice(&CWD_MAGIC.to_le_bytes());

    (payload, argc, fd_inherit_offset)
}

/// Build a `CONTAINER_RUN` payload with argv, FdInherit entries, REDIR entries,
/// AND an ENV block.
///
/// Wire format (in order):
///   `[name_bytes]`
///   `[u32 FD_INHERIT_MAGIC LE][u32 count LE][(u32 fd, u32 flags, u64 ep, u64 vfs_client_id, u64 vfs_remote_fd) * count]`
///   `[argv[0]\0][argv[1]\0]...[u32 argv_bytes_len LE][u32 ARGV_MAGIC LE]`
///   `[redir entries...][u32 entries_len LE][u32 REDIR_MAGIC LE]`
///   `[env_bytes][u32 env_bytes_len LE][u32 ENV_MAGIC LE]`
///   `[cwd_bytes][u32 cwd_len LE][u32 CWD_MAGIC LE]`
///
/// Stripping order in procmgr (outermost first): CWD → ENV → REDIR → ARGV,
/// leaving `[name][FdInherit]` in `effective_payload`.
///
/// `env` carries the parent's exported env layered with the shell's exported
/// vars; entries are packed as "KEY=VALUE\0" — same wire format as procmgr's
/// `build_default_env_payload`. An empty `env` slice omits the ENV trailer
/// (procmgr falls back to `DEFAULT_ENV`).
///
/// Returns `(payload_bytes, argc, fd_inherit_offset)`.
/// Maximum 4 redirs; paths capped at 255 bytes each.
#[cfg(feature = "posix")]
pub fn build_container_run_payload_full(
    name: &str,
    args: &[&str],
    fd_inherit: &[FdInherit],
    redirs: &[RedirAction],
    env: &[(&str, &str)],
) -> (Vec<u8>, usize, usize) {
    use crate::boot::CWD_MAX;

    let argc = args.len();
    let argv_bytes_est: usize = args.iter().map(|a| a.len() + 1).sum();
    let redir_bytes_est: usize = redirs.iter().map(|r| 4 + r.path.len().min(255)).sum();
    let env_bytes_est: usize = env.iter().map(|(k, v)| k.len() + v.len() + 2).sum();
    let mut payload =
        Vec::with_capacity(name.len() + argv_bytes_est + 32 * fd_inherit.len() + redir_bytes_est + env_bytes_est + 24 + CWD_MAX + 24);
    payload.extend_from_slice(name.as_bytes());

    // FdInherit blob immediately after the image name.
    let fd_inherit_offset = if fd_inherit.is_empty() { 0 } else { payload.len() };
    if !fd_inherit.is_empty() {
        // Magic value 0x46444143 spells "FDAC" in ASCII — kept for historical
        // wire-format compatibility; the concept has since been renamed FdInherit.
        const FD_INHERIT_MAGIC: u32 = 0x46444143;
        payload.extend_from_slice(&FD_INHERIT_MAGIC.to_le_bytes());
        payload.extend_from_slice(&(fd_inherit.len() as u32).to_le_bytes());
        for entry in fd_inherit {
            // bytes 0–3: target_fd
            payload.extend_from_slice(&entry.target_fd.to_le_bytes());
            // bytes 4–7: flags
            let flags: u32 = if entry.is_pipe { 0x01 } else { 0 };
            payload.extend_from_slice(&flags.to_le_bytes());
            // bytes 8–15: endpoint
            payload.extend_from_slice(&(entry.endpoint as u64).to_le_bytes());
            // bytes 16–23: vfs_client_id
            payload.extend_from_slice(&(entry.vfs_client_id as u64).to_le_bytes());
            // bytes 24–31: vfs_remote_fd
            payload.extend_from_slice(&(entry.vfs_remote_fd as u64).to_le_bytes());
        }
    }

    // ARGV trailer comes after FdInherit.
    if argc > 0 {
        let argv_start = payload.len();
        for arg in args {
            payload.extend_from_slice(arg.as_bytes());
            payload.push(0);
        }
        let argv_bytes_len = (payload.len() - argv_start) as u32;
        payload.extend_from_slice(&argv_bytes_len.to_le_bytes());
        payload.extend_from_slice(&ARGV_MAGIC.to_le_bytes());
    }

    // REDIR trailer comes after ARGV, before ENV.
    if !redirs.is_empty() {
        let redir_start = payload.len();
        for r in redirs.iter().take(4) {
            let path_bytes = r.path.as_bytes();
            let path_len = path_bytes.len().min(255) as u16;
            payload.push(r.target_fd);
            payload.push(r.flags);
            payload.extend_from_slice(&path_len.to_le_bytes());
            payload.extend_from_slice(&path_bytes[..path_len as usize]);
        }
        let entries_len = (payload.len() - redir_start) as u32;
        payload.extend_from_slice(&entries_len.to_le_bytes());
        payload.extend_from_slice(&REDIR_MAGIC.to_le_bytes());
    }

    // ENV trailer comes after REDIR, before CWD.
    if !env.is_empty() {
        let env_start = payload.len();
        for (k, v) in env {
            payload.extend_from_slice(k.as_bytes());
            payload.push(b'=');
            payload.extend_from_slice(v.as_bytes());
            payload.push(0);
        }
        let env_bytes_len = (payload.len() - env_start) as u32;
        payload.extend_from_slice(&env_bytes_len.to_le_bytes());
        payload.extend_from_slice(&ENV_MAGIC.to_le_bytes());
    }

    // CWD trailer is always last.
    let cwd_string = crate::posix::current_dir_string();
    let cwd_bytes = cwd_string.as_bytes();
    let cwd_len = cwd_bytes.len().min(CWD_MAX);
    payload.extend_from_slice(&cwd_bytes[..cwd_len]);
    payload.extend_from_slice(&(cwd_len as u32).to_le_bytes());
    payload.extend_from_slice(&CWD_MAGIC.to_le_bytes());

    (payload, argc, fd_inherit_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_ring_roundtrip_wraps() {
        let mut backing = [0u8; 96];
        let mut ring = SharedRing::initialize(&mut backing).expect("init");
        assert_eq!(ring.capacity(), 64); // 32-byte header + 64-byte data

        let first = [1u8; 40];
        assert_eq!(ring.push(&first), 40);
        assert_eq!(ring.available_read(), 40);

        let mut out = [0u8; 24];
        assert_eq!(ring.pop(&mut out), 24);
        assert_eq!(out, [1u8; 24]);

        let second = [2u8; 30];
        assert_eq!(ring.push(&second), 30);

        let mut drain = [0u8; 46];
        assert_eq!(ring.pop(&mut drain), 46);
        assert_eq!(&drain[..16], [1u8; 16].as_slice());
        assert_eq!(&drain[16..], [2u8; 30].as_slice());
    }

    #[test]
    fn shared_ring_attach_rejects_uninitialized() {
        let mut backing = [0u8; 128];
        assert_eq!(
            SharedRing::attach(&mut backing).err(),
            Some(Error::InvalidState)
        );
    }

    #[test]
    fn shared_ring_bytes_overflow_guard() {
        assert_eq!(
            SharedRing::bytes_for_capacity(usize::MAX).err(),
            Some(Error::Overflow)
        );
    }
}
