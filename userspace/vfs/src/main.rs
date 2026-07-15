#![no_std]
#![no_main]

//! Virtual Filesystem Service for CLUU.
//!
//! Mount points are declared declaratively in `setup_mounts()`.
//! All path routing is handled by the unified MountTable.

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cmp;
use core::mem::size_of;
use libcluu::elf::{ElfFile, LoadableSegment};
use libcluu::fs::protocol::{
    VfsOp, VFS_CLOSE, VFS_FSTAT, VFS_LINK, VFS_MAP_ELF, VFS_MKDIR, VFS_OPEN, VFS_READDIR,
    VFS_BOUNCE_SETUP, VFS_READ_GRANT, VFS_READ_RING, VFS_REALPATH, VFS_RENAME, VFS_RING_SETUP, VFS_RMDIR, VFS_STAT,
    VFS_UNLINK, VFS_WRITE,
};
use libcluu::ipc::{
    self, extract_reply_id, parse_message, reply_with_payload, SharedRing, SharedRingHeader,
    PTS_REGISTER_LABEL, PTS_UNREGISTER_LABEL, VFS_DERIVE_CHILD_FD_LABEL,
};
use libcluu::types::Message;
use libcluu::*;

mod bulk_pool;
mod fd_table;
pub mod memfs;
mod mount;
mod procfs;
mod pts;
mod view;

use fd_table::{FdTable, OpenFile};
use mount::MountTable;
use mount::{DirEntry, DirEntryStat};

use libcluu::async_runtime::{IpcCallFuture, Runtime};
use libcluu::boot::TOKEN_EXTRA_0;
use libcluu::boot::{PARAM_ARGC, PARAM_ARGV_OFFSET, PROCESS_INFO_ADDR};
use libcluu::mem::PAGE_SIZE;
use procmgr_common::wire::SessionEnvelope;
// Accommodate full set_view payloads (13B header + src/dst bytes per mount,
// now ~10+ entries per container after the mount-policy atomic flip).
const IPC_MESSAGE_MAX: usize = 4096;
/// Remote filesystem IPC label for zero-copy reads into the VFS grant buffer.
const FS_READ_GRANT: u32 = 0x306;
/// Remote filesystem IPC label for write operations.
const FS_WRITE: u32 = 0x305;
const USIZE_BYTES: usize = size_of::<usize>();
const TWO_USIZE_BYTES: usize = size_of::<usize>() * 2;

enum VfsCompletion {
    Open { reply_token: usize, client_id: usize, result: Result<OpenFile> },
    Readdir { reply_token: usize, client_id: usize, result: Result<Vec<DirEntry>> },
    Stat { reply_token: usize, client_id: usize, result: Result<DirEntryStat> },
    #[allow(dead_code)]
    // rationale: Read/Write/CreateFile completions for future async VFS ops.
    Read { reply_token: usize, #[allow(dead_code)] client_id: usize, result: Result<Vec<u8>> },
    #[allow(dead_code)]
    Write { reply_token: usize, #[allow(dead_code)] client_id: usize, result: Result<usize> },
    Unlink { reply_token: usize, result: Result<()> },
    Mkdir { reply_token: usize, result: Result<()> },
    Rmdir { reply_token: usize, result: Result<()> },
    Rename { reply_token: usize, result: Result<()> },
    Link { reply_token: usize, result: Result<()> },
    #[allow(dead_code)]
    CreateFile { reply_token: usize, result: Result<()> },
    Realpath { reply_token: usize, result: Result<String> },
    TtyReadGrant { reply_token: usize, target_base: usize, target_space: usize, result: Result<Vec<u8>> },
    TtyReadRing { reply_token: usize, client_id: usize, max_fill: usize, result: Result<Vec<u8>> },
    PtsVerb { reply_token: usize, label: u32, result: Result<Vec<u8>> },
}

// VFS region layout — kept at the legacy [0x6000_0000, 0x7900_0000) range.
//
// Empirically a relocation to [0x9000_0000, 0xE100_0000) breaks small-file
// VFS reads from procmgr (e.g. /var/images/vt/manifest.toml returns size=0
// silently between open and load_from_vfs_ring) even though every region
// fits within the new USER_GRANT_TOP = 0x1_0000_0000 cap and grant_buf is
// well within range. Root cause not isolated yet — relocation is therefore
// gated on understanding that interaction. The kernel-side grant-cap bump
// is still useful: it removes the structural ceiling so a future surgical
// move (e.g. just CACHE_BUF_BASE up to 0x8400_0000 for 256 MB headroom)
// becomes possible without touching grant or ring placement.
//
// Don't push CACHE_BUF_SIZE past 128 MB until ring/bounce are moved up too:
// 0x6400_0000 + 256 MB = 0x7400_0000 collides with RING_POOL_BASE.

/// Buffer base for file data reads (shared grant window).
const READ_BUF_BASE: usize = 0x60000000;
/// Size of the shared grant window in the VFS address space.
/// 4MB to reduce IPC round-trips for cold ELF cache fills.
const GRANT_BUF_SIZE: usize = 4 * 1024 * 1024;
/// Buffer base for the VFS read cache region.
const CACHE_BUF_BASE: usize = 0x64000000;
/// Size of the VFS read cache region. 256 MB headroom at the VA level —
/// eviction is disabled while MAP_SHARE_PHYS shares are pinned, so the
/// budget must hold every binary that may ever be cached. Sum of
/// containers in `/var/images` is well under 100 MB at v1; 128 MB leaves
/// room without overflowing the 256 MB gap before RING_POOL_BASE.
const CACHE_BUF_SIZE: usize = 128 * 1024 * 1024;
/// Dedicated per-client shared-ring pool (for ring bulk reads).
const RING_POOL_BASE: usize = 0x74000000;
const RING_SLOT_BYTES: usize = 64 * 1024;
const RING_SLOT_COUNT: usize = 4;
const RING_POOL_SIZE: usize = RING_SLOT_BYTES * RING_SLOT_COUNT;
const RING_SLOT_CAPACITY: usize = RING_SLOT_BYTES - SharedRingHeader::bytes();
const RING_MIN_REQUESTED_BYTES: usize = 8 * 1024;

/// Per-client bounce-buffer pool for big single-shot reply payloads
/// (readdir blobs, etc.). Each slot is a flat 64 KiB shared frame mapped
/// into VFS at `BOUNCE_POOL_BASE + slot * BOUNCE_SLOT_BYTES` and granted
/// to the client at its requested target_base. Reply carries
/// `(used_bounce, len)`; client memcpys out. One outstanding RPC per
/// client (synchronous IPC), so overwrite-on-reply is safe.
const BOUNCE_POOL_BASE: usize = 0x78000000;
const BOUNCE_SLOT_BYTES: usize = 64 * 1024;
const BOUNCE_SLOT_COUNT: usize = 16;

// ── VFS view-manager cap scope masks ─────────────────────────────────────────
// Each bit represents a mount-root that a sub-minted cap may install views for.
const VIEW_SCOPE_ROOT: u16 = 1 << 0;
const VIEW_SCOPE_DEV: u16 = 1 << 1;
const VIEW_SCOPE_VAR_IMAGES: u16 = 1 << 2;
const VIEW_SCOPE_HOME: u16 = 1 << 3;
const VIEW_SCOPE_TMP: u16 = 1 << 4;
// bits 5–15 reserved
pub const VIEW_SCOPE_ALL: u16 =
    VIEW_SCOPE_ROOT | VIEW_SCOPE_DEV | VIEW_SCOPE_VAR_IMAGES | VIEW_SCOPE_HOME | VIEW_SCOPE_TMP;

// Object type tag for ObjectRef::VfsViewManager in the kernel wire encoding.
const TOKEN_TYPE_VFS_VIEW_MANAGER: u8 = 0x09;

// Word slot carrying the view-manager cap handle in VFS_SET_VIEW and
// VFS_CONTAINER_CLEANUP messages.  words[0..4] are used by set_view;
// words[1..2] are used by container_cleanup.  words[5] is free in both.
const VIEW_MGR_CAP_WORD: usize = 5;

/// Resolve `handle` to its VfsViewManager scope fields via TokenGetInfo.
///
/// Returns `Ok((scope_sid, scope_mask))` on success.
/// Returns `Err(PermissionDenied)` if the handle is 0, invalid, or wrong type.
fn resolve_view_mgr_cap(handle: usize) -> core::result::Result<(u32, u16), libcluu::Error> {
    if handle == 0 {
        return Err(libcluu::Error::PermissionDenied);
    }
    let (type_tag, scope_sid, scope_mask) =
        libcluu::syscall::token_get_info(handle).map_err(|_| libcluu::Error::PermissionDenied)?;
    if type_tag != TOKEN_TYPE_VFS_VIEW_MANAGER {
        return Err(libcluu::Error::PermissionDenied);
    }
    Ok((scope_sid, scope_mask))
}
const BOUNCE_POOL_SIZE: usize = BOUNCE_SLOT_BYTES * BOUNCE_SLOT_COUNT;
/// Cap for remote grant reads to avoid large transient allocations.
const REMOTE_READ_CAP: usize = GRANT_BUF_SIZE;
/// Maximum size of file to cache. 32 MB covers any single CLUU binary
/// in v1 (largest: ls/edit ~5 MB, headroom for unoptimized builds).
const FILE_CACHE_MAX_SIZE: usize = 32 * 1024 * 1024;
/// Maximum total cache size — must equal CACHE_BUF_SIZE.
const FILE_CACHE_TOTAL_MAX: usize = CACHE_BUF_SIZE;
const VFS_TRACE: bool = false;

// POSIX-style mode bits (matching libcluu::posix::stat)
const S_IFREG: usize = 0o100000;
const S_IFDIR: usize = 0o040000;
const MODE_FILE: usize = S_IFREG | 0o644;
const S_IFCHR: usize = 0o020000;
const MODE_DIR: usize = S_IFDIR | 0o755;
const O_WRONLY: usize = 1;
const O_RDWR: usize = 2;
const O_CREAT: usize = 0o1000; // newlib _FCREAT = 0x0200
const O_EXCL: usize = 0o4000; // newlib _FEXCL = 0x0800
const O_TRUNC: usize = 0o2000; // newlib _FTRUNC = 0x0400

/// Full stat result (v2 wire format).  Mirrors VfsStat in libcluu.
#[derive(Clone, Copy, Default)]
struct StatInfo {
    size:   u64,
    mode:   u32,
    mtime:  u64,
    nlink:  u32,
    uid:    u32,
    gid:    u32,
    blocks: u64,
}

impl StatInfo {
    fn to_bytes(self) -> [u8; 40] {
        let mut buf = [0u8; 40];
        buf[0..8].copy_from_slice(&self.size.to_le_bytes());
        buf[8..12].copy_from_slice(&self.mode.to_le_bytes());
        buf[12..20].copy_from_slice(&self.mtime.to_le_bytes());
        buf[20..24].copy_from_slice(&self.nlink.to_le_bytes());
        buf[24..28].copy_from_slice(&self.uid.to_le_bytes());
        buf[28..32].copy_from_slice(&self.gid.to_le_bytes());
        buf[32..40].copy_from_slice(&self.blocks.to_le_bytes());
        buf
    }
}

fn op_reply_label(op: VfsOp) -> u32 {
    match op {
        VfsOp::Open => VFS_OPEN,
        VfsOp::Close => VFS_CLOSE,
        VfsOp::ReadGrant => VFS_READ_GRANT,
        VfsOp::Readdir => VFS_READDIR,
        VfsOp::MapElf => VFS_MAP_ELF,
        VfsOp::Write => VFS_WRITE,
        VfsOp::Stat => VFS_STAT,
        VfsOp::Fstat => VFS_FSTAT,
        VfsOp::Unlink => VFS_UNLINK,
        VfsOp::Mkdir => VFS_MKDIR,
        VfsOp::Rmdir => VFS_RMDIR,
        VfsOp::Rename => VFS_RENAME,
        VfsOp::RingSetup => VFS_RING_SETUP,
        VfsOp::ReadRing => VFS_READ_RING,
        VfsOp::Link => VFS_LINK,
        VfsOp::Realpath => VFS_REALPATH,
        VfsOp::BounceSetup => VFS_BOUNCE_SETUP,
    }
}

macro_rules! vfs_trace {
    ($($arg:tt)*) => {
        if VFS_TRACE {
            let _ = debug_print(&format!($($arg)*));
        }
    };
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = run_vfs() {
        let _ = debug_print(&format!("vfs: fatal error {:?}", err));
        return 1;
    }
    0
}

fn run_vfs() -> Result<()> {
    debug_print("vfs: starting...")?;

    let info = process_info();
    let endpoint = info.tokens[TOKEN_EXTRA_0];
    let space_token = info.tokens[TOKEN_SPACE];

    let session_envelope = read_session_envelope();
    let session_sid = session_envelope.as_ref().map(|e| e.sid);
    let is_session = session_sid.is_some();

    if is_session {
        let _ = debug_print(&alloc::format!("vfs: session mode sid={}", session_sid.unwrap()));
    }

    let initrd_size = if is_session { 0 } else { info.params[PARAM_INITRD_SIZE] as usize };
    let initrd: &'static [u8] = if initrd_size > 0 {
        map_initrd_slice(initrd_size)
    } else {
        &[][..]
    };

    let fb_phys = if is_session { 0 } else { info.params[PARAM_VFS_FB_PHYS] };
    let fb_size = if is_session { 0 } else { info.params[PARAM_VFS_FB_SIZE] };
    let fb_width = if is_session { 0 } else { info.params[PARAM_VFS_FB_WIDTH] };
    let fb_height = if is_session { 0 } else { info.params[PARAM_VFS_FB_HEIGHT] };
    let fb_pitch = if is_session { 0 } else { info.params[PARAM_VFS_FB_PITCH] };
    procfs::set_fb_info(procfs::FbInfo {
        phys: fb_phys,
        size: fb_size,
        width: fb_width,
        height: fb_height,
        pitch: fb_pitch,
    });

    // Build FbInfo for /dev/fb0 — only if the framebuffer is present.
    let dev_fb_info = if fb_phys != 0 {
        Some(mount::FbInfo {
            phys: fb_phys,
            size: fb_size,
            width: fb_width as u32,
            height: fb_height as u32,
            pitch: fb_pitch as u32,
            bpp: 32, // CLUU framebuffer is BGRA32
        })
    } else {
        None
    };

    if is_session {
        let sid = session_sid.unwrap();
        registry::init("session-vfs")?;
        registry::register_default_outputs()?;
        let output_name = alloc::format!("main:{}", sid);
        registry::register_output(&output_name, endpoint)?;
        let _ = registry::request_subscription("session-vfs", &output_name);
    } else {
        debug_print("vfs: registering...")?;
        registry::init("vfs")?;
        registry::register_default_outputs()?;
        registry::register_output("main", endpoint)?;
    }

    debug_print("vfs: waiting for services...")?;

    // Wait for services to start
    for _ in 0..100 {
        yield_cpu()?;
    }

    // Setup mount points.  In session mode /proc is backed by session-procmgr.
    let procmgr_endpoint = if is_session {
        let sid = session_sid.unwrap();
        let ep_name = alloc::format!("main:{}", sid);
        registry::subscribe_output("session-procmgr", &ep_name)?
    } else {
        registry::subscribe_output("root-procmgr", "spawn")?
    };
    let mounts = setup_mounts(initrd, dev_fb_info, procmgr_endpoint)?;

    if !is_session {
        registry::register_output("mounted", endpoint)?;
        debug_print("vfs: published 'mounted' signal")?;
    }

    let grant_buf_base = map_grant_buffer(space_token)?;
    let _ = debug_print(&format!(
        "vfs: grant buffer mapped base={:#x} size={}",
        grant_buf_base, GRANT_BUF_SIZE
    ));
    let cache_buf_base = map_cache_buffer(space_token)?;
    let _ = debug_print(&format!(
        "vfs: cache buffer mapped base={:#x} size={}",
        cache_buf_base, CACHE_BUF_SIZE
    ));
    let ring_pool_base = map_ring_pool(space_token)?;
    let _ = debug_print(&format!(
        "vfs: ring pool mapped base={:#x} size={}",
        ring_pool_base, RING_POOL_SIZE
    ));
    let bounce_pool_base = map_bounce_pool(space_token)?;
    let _ = debug_print(&format!(
        "vfs: bounce pool mapped base={:#x} size={}",
        bounce_pool_base, BOUNCE_POOL_SIZE
    ));
    let vfs_space_map_token =
        token_derive(space_token, Rights::SPACE_MAP.bits() as usize, u64::MAX)?;
    let mut server = VfsServer::new(
        endpoint,
        space_token,
        vfs_space_map_token,
        info.tokens[TOKEN_CLOCK],
        grant_buf_base,
        GRANT_BUF_SIZE,
        cache_buf_base,
        CACHE_BUF_SIZE,
        ring_pool_base,
        RING_POOL_SIZE,
        bounce_pool_base,
        BOUNCE_POOL_SIZE,
        mounts,
        session_sid.map(|s| s as u32),
    );
    let registry_endpoint = registry::control_endpoint();
    // Heap-allocated IPC buffer: keeps the 4KB receive buffer off the VFS
    // service stack (64KB), reducing stack pressure during deep handler call
    // chains under heavy client churn (terminal flood scenario).
    let mut buf = vec![0u8; IPC_MESSAGE_MAX].into_boxed_slice();

    if !is_session {
        server.preload_marked_binaries();
    }

    debug_print("vfs: ready")?;

    // Subscribe to tty:N main endpoints (root-VFS only).
    if !is_session {
        for i in 0..4usize {
            let name = alloc::format!("tty:{}", i);
            let _ = registry::request_subscription(&name, "main");
        }
    }

    let token_self = info.tokens[TOKEN_SELF];
    let mut runtime = Runtime::new(token_self)?;
    let reply_ep = runtime.reply_endpoint();

    loop {
        runtime.poll_ready();

        while let Some(comp) = runtime.pop_completion() {
            if let Ok(vfs_comp) = comp.downcast::<VfsCompletion>() {
                match *vfs_comp {
                    VfsCompletion::Open { reply_token, client_id, result } => {
                        server.complete_async_open(reply_token, client_id, result);
                    }
                    VfsCompletion::Readdir { reply_token, client_id, result } => {
                        server.complete_async_readdir(reply_token, client_id, result);
                    }
                    VfsCompletion::Stat { reply_token, client_id: _, result } => {
                        server.complete_async_stat(reply_token, result);
                    }
                    VfsCompletion::Read { reply_token, client_id: _, result } => {
                        server.complete_async_read(reply_token, result);
                    }
                    VfsCompletion::Write { reply_token, client_id: _, result } => {
                        server.complete_async_write(reply_token, result);
                    }
                    VfsCompletion::Unlink { reply_token, result } => {
                        server.complete_async_simple(reply_token, VFS_UNLINK, result);
                    }
                    VfsCompletion::Mkdir { reply_token, result } => {
                        server.complete_async_simple(reply_token, VFS_MKDIR, result);
                    }
                    VfsCompletion::Rmdir { reply_token, result } => {
                        server.complete_async_simple(reply_token, VFS_RMDIR, result);
                    }
                    VfsCompletion::Rename { reply_token, result } => {
                        server.complete_async_simple(reply_token, VFS_RENAME, result);
                    }
                    VfsCompletion::Link { reply_token, result } => {
                        server.complete_async_simple(reply_token, VFS_LINK, result);
                    }
                    VfsCompletion::CreateFile { reply_token, result } => {
                        server.complete_async_simple(reply_token, VFS_OPEN, result);
                    }
                    VfsCompletion::Realpath { reply_token, result } => {
                        server.complete_async_realpath(reply_token, result);
                    }
                    VfsCompletion::TtyReadGrant { reply_token, target_base, target_space, result } => {
                        server.complete_tty_read_grant(reply_token, target_base, target_space, result);
                    }
                    VfsCompletion::TtyReadRing { reply_token, client_id, max_fill, result } => {
                        server.complete_tty_read_ring(reply_token, client_id, max_fill, result);
                    }
                    VfsCompletion::PtsVerb { reply_token, label, result } => {
                        server.complete_pts_verb(reply_token, label, result);
                    }
                }
            }
        }

        let tokens = [endpoint, registry_endpoint, reply_ep];
        match libcluu::syscall::ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok((index, len, sender_tid)) => {
                if index == 2 {
                    if let Some((msg, _payload)) = parse_message(&buf[..len]) {
                        let cookie = msg.words[5];
                        let payload_start = size_of::<Message>();
                        let payload_bytes: Vec<u8> = if len > payload_start {
                            buf[payload_start..len].to_vec()
                        } else {
                            Vec::new()
                        };
                        runtime.deliver_reply(cookie, msg, payload_bytes);
                    }
                } else if index == 1 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        server.handle_registry_message(&msg, payload, sender_tid);
                    }
                } else if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if let Err(err) = server.handle_message(&msg, payload, sender_tid, &mut runtime) {
                        vfs_trace!("vfs: handler error {:?}", err);
                    }
                }
            }
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {
                let _ = libcluu::yield_cpu();
            }
            Err(e) => return Err(e),
        }
    }
}

fn read_session_envelope() -> Option<SessionEnvelope> {
    let info = process_info();
    let byte_len = info.params[PARAM_ARGC] as usize;
    let byte_off = info.params[PARAM_ARGV_OFFSET] as usize;
    if byte_len == 0 || byte_off == 0 || byte_off + byte_len > PAGE_SIZE {
        return None;
    }
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);
    let ptr = (page_base + byte_off) as *const u8;
    let raw = unsafe { core::slice::from_raw_parts(ptr, byte_len) };
    postcard::from_bytes(raw).ok()
}

fn setup_mounts(
    initrd: &'static [u8],
    fb_info: Option<mount::FbInfo>,
    procmgr_endpoint: usize,
) -> Result<MountTable> {
    debug_print("vfs: setup_mounts start")?;
    let mut mounts = MountTable::new();

    // ═══════════════════════════════════════════════════════════════════════
    // Mount points configuration
    // ═══════════════════════════════════════════════════════════════════════

    // Initrd: system files from boot archive
    mounts.mount_initrd("/dev/initrd", initrd);
    debug_print("vfs: initrd mounted")?;

    // Ext2 filesystem: forwarded to virtio-blk service (mounted as root).
    let blkdev_endpoint = registry::subscribe_output("blkdev", "main")?;
    mounts.mount_remote("/", blkdev_endpoint, "blkdev");
    debug_print("vfs: mounted / (blkdev)")?;

    mounts.mount_async("/proc", alloc::boxed::Box::new(procfs::ProcfsBackend::new(procmgr_endpoint)));
    debug_print("vfs: mounted /proc (procfs)")?;

    // Device files: /dev/null, /dev/zero, /dev/urandom, /dev/tty*, /dev/fb0
    let mut dev_backend = mount::DeviceBackend::new();
    if let Some(fb_info) = fb_info {
        dev_backend.set_fb(fb_info);
    }
    mounts.mount_sync("/dev", alloc::boxed::Box::new(dev_backend));
    debug_print("vfs: mounted /dev (devfs)")?;

    let hostfs_endpoint = match registry::subscribe_output("hostfs", "main") {
        Ok(ep) => ep,
        Err(e) => {
            debug_print(&format!("vfs: hostfs not available ({:?}), /host disabled", e))?;
            0
        }
    };
    if hostfs_endpoint != 0 {
        mounts.mount_remote("/host", hostfs_endpoint, "hostfs");
        debug_print("vfs: mounted /host (hostfs)")?;
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Future mount points can be added here:
    // - mounts.mount_virtual("/sys", "sysfs", sysfs::ENTRIES);
    // - mounts.mount_remote("/net", netfs_endpoint, "netfs");
    // - mounts.mount_initrd("/boot", boot_archive);
    // ═══════════════════════════════════════════════════════════════════════

    Ok(mounts)
}

fn map_initrd_slice(initrd_size: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, initrd_size) }
}

/// Simple LRU-ish file cache for ext2 files.
/// Caches entire file contents by (inode, size) to avoid repeated disk reads.
struct FileCache {
    /// Map from (inode, size) to cached entry.
    entries: BTreeMap<(u32, usize), CacheEntry>,
    /// Optional parsed ELF metadata for cached files keyed by (inode, size).
    elf_meta: BTreeMap<(u32, usize), CachedElfMeta>,
    /// Total bytes currently cached.
    total_size: usize,
    /// Access counter for LRU ordering.
    access_counter: usize,
    /// Dedicated cache region backing the entries.
    region: CacheRegion,
}

#[derive(Clone, Copy)]
struct CacheEntry {
    base: usize,
    len: usize,
    last_access: usize,
}

#[derive(Clone)]
struct CachedElfMeta {
    entry_point: usize,
    segments: Vec<CachedElfSegment>,
}

#[derive(Clone, Copy)]
struct CachedElfSegment {
    vaddr: usize,
    mem_size: usize,
    file_offset: usize,
    file_size: usize,
    page_flags: usize,
}

struct CacheRegion {
    base: usize,
    size: usize,
    offset: usize,
    free: Vec<FreeBlock>,
}

struct FreeBlock {
    base: usize,
    size: usize,
}

/// A shell read(2) parked by the async PTS read path.
///
/// Created in `handle_read_grant` (pts arm) instead of sync-calling cluuterm.
/// The shell stays blocked on `reply_token`.  When cluuterm replies with
/// `PTS_READ_DELIVER_LABEL`, VFS pops this, grants the bytes into `target_base`
/// inside `caller_space`, then replies `reply_token` to unblock the shell.
struct ParkedRead {
    /// Reply slot that unblocks the shell when answered (sync path).
    reply_token: usize,
    /// Async reply endpoint (0 = sync call; non-zero = async IpcCallFuture caller).
    reply_ep: usize,
    /// Async correlation cookie (valid only when reply_ep != 0).
    cookie: usize,
    /// Space token of the shell process (for `space_grant`).
    caller_space: usize,
    /// Grant target base address in the shell's address space.
    target_base: usize,
    /// Maximum bytes the shell requested.
    requested: usize,
}

fn reply_parked(parked: &ParkedRead, reply_msg: &Message, payload: &[u8]) -> Result<()> {
    if parked.reply_ep != 0 {
        let mut msg = reply_msg.clone();
        msg.words[0] = payload.len();
        msg.words[libcluu::ipc::ASYNC_REPLY_COOKIE_WORD] = parked.cookie;
        if payload.is_empty() {
            ipc::send(parked.reply_ep, &msg, IpcFlags::empty())
        } else {
            ipc::send_msg_with_payload(parked.reply_ep, &msg, payload)
        }
    } else {
        if payload.is_empty() {
            ipc::reply(parked.reply_token, reply_msg, IpcFlags::empty())
        } else {
            ipc::reply_with_payload(parked.reply_token, reply_msg, payload)
        }
    }
}

impl FileCache {
    fn new(base: usize, size: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            elf_meta: BTreeMap::new(),
            total_size: 0,
            access_counter: 0,
            region: CacheRegion::new(base, size),
        }
    }

    fn get(&mut self, inode: u32, size: usize) -> Option<CacheEntry> {
        let key = (inode, size);
        if let Some(entry) = self.entries.get_mut(&key) {
            self.access_counter += 1;
            entry.last_access = self.access_counter;
            Some(*entry)
        } else {
            None
        }
    }

    fn reserve(&mut self, size: usize) -> Option<usize> {
        if size == 0 || size > FILE_CACHE_MAX_SIZE || size > FILE_CACHE_TOTAL_MAX {
            return None;
        }
        // FIXME(MAP_SHARE_PHYS): evict_lru is a no-op while shared frames are
        // pinned, so we check the budget once and return None if full rather
        // than looping forever.
        if self.total_size + size > FILE_CACHE_TOTAL_MAX {
            return None;
        }
        self.region.allocate(size)
    }

    fn insert(&mut self, inode: u32, size: usize, base: usize, len: usize) {
        self.access_counter += 1;
        self.total_size += len;
        self.entries.insert(
            (inode, size),
            CacheEntry {
                base,
                len,
                last_access: self.access_counter,
            },
        );
    }

    fn release_reserved(&mut self, base: usize, len: usize) {
        self.region.free(base, len);
    }

    fn remove(&mut self, inode: u32, size: usize) {
        let key = (inode, size);
        if let Some(entry) = self.entries.remove(&key) {
            self.total_size = self.total_size.saturating_sub(entry.len);
            self.region.free(entry.base, entry.len);
        }
        self.elf_meta.remove(&key);
    }

    fn get_or_build_elf_meta(
        &mut self,
        inode: u32,
        size: usize,
        data: &[u8],
    ) -> Result<CachedElfMeta> {
        let key = (inode, size);
        if let Some(meta) = self.elf_meta.get(&key) {
            return Ok(meta.clone());
        }

        let elf = ElfFile::parse(data).map_err(|_| Error::InvalidArgument)?;
        let mut segments = Vec::new();
        for segment in elf.segments_iter() {
            segments.push(CachedElfSegment {
                vaddr: segment.vaddr as usize,
                mem_size: segment.mem_size as usize,
                file_offset: segment.file_offset as usize,
                file_size: segment.file_size as usize,
                page_flags: segment.page_flags() as usize,
            });
        }

        let meta = CachedElfMeta {
            entry_point: elf.entry_point as usize,
            segments,
        };
        self.elf_meta.insert(key, meta.clone());
        Ok(meta)
    }

    #[allow(dead_code)]
    fn evict_lru(&mut self) {
        // FIXME(MAP_SHARE_PHYS): cache eviction disabled while shares are pinned.
        // Revisit when refcount lands. For v1 the 32 MB cache budget is enough
        // for all CLUU userspace binaries (sum < 30 MB).
        let _ = self; // suppress unused-mut warning
    }
}

impl CacheRegion {
    fn new(base: usize, size: usize) -> Self {
        Self {
            base,
            size,
            offset: 0,
            free: Vec::new(),
        }
    }

    fn allocate(&mut self, size: usize) -> Option<usize> {
        // Align to PAGE_SIZE so that MAP_SHARE_PHYS can share physical frames
        // without sub-page offset issues.  Each cache entry's base must be
        // page-aligned so that `data_ptr + segment.file_offset` (where
        // file_offset is always page-aligned for well-formed ELFs) is also
        // page-aligned.
        let size = align_up(size, PAGE_SIZE);
        if let Some(index) = self.free.iter().position(|b| b.size >= size) {
            let block = self.free.remove(index);
            let base = block.base;
            let remaining = block.size - size;
            if remaining > 0 {
                self.free.push(FreeBlock {
                    base: block.base + size,
                    size: remaining,
                });
            }
            return Some(base);
        }

        if self.offset + size > self.size {
            return None;
        }
        let base = self.base + self.offset;
        self.offset += size;
        Some(base)
    }

    fn free(&mut self, base: usize, size: usize) {
        let size = align_up(size, PAGE_SIZE);
        self.free.push(FreeBlock { base, size });
        self.coalesce();
    }

    fn coalesce(&mut self) {
        if self.free.len() < 2 {
            return;
        }
        self.free.sort_by_key(|b| b.base);
        let mut merged: Vec<FreeBlock> = Vec::with_capacity(self.free.len());
        for block in self.free.drain(..) {
            if let Some(last) = merged.last_mut() {
                if last.base + last.size == block.base {
                    last.size += block.size;
                    continue;
                }
            }
            merged.push(block);
        }
        self.free = merged;
    }
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) & !(align - 1)
}

/// Cheap textual scan for `preload = true` in a manifest TOML. Avoids
/// pulling in a full TOML parser at VFS startup; the manifest is
/// generated by `container-build` so the exact line shape is stable.
fn manifest_has_preload(manifest: &str) -> bool {
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "preload = true" || trimmed == "preload=true" {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy)]
struct ReadRingSession {
    source_base: usize,
    target_base: usize,
    target_space: usize,
    bytes: usize,
    capacity: usize,
    slot: usize,
}

/// Default quota for per-container ephemeral MemFs (4 MiB).
const DEFAULT_MEMFS_QUOTA: usize = 4 * 1024 * 1024;

/// Single-path mutation kind: discriminator for handle_single_path_mutation.
#[derive(Clone, Copy)]
enum SinglePathOp {
    Unlink,
    Mkdir { mode: usize },
    Rmdir,
}

impl SinglePathOp {
    fn name(&self) -> &'static str {
        match self {
            Self::Unlink => "unlink",
            Self::Mkdir { .. } => "mkdir",
            Self::Rmdir => "rmdir",
        }
    }

    fn reply_label(&self) -> u32 {
        match self {
            Self::Unlink => VFS_UNLINK,
            Self::Mkdir { .. } => VFS_MKDIR,
            Self::Rmdir => VFS_RMDIR,
        }
    }
}

struct VfsServer {
    endpoint: usize,
    space_token: usize,
    vfs_space_map_token: usize,
    grant_buf_base: usize,
    grant_buf_size: usize,
    ring_pool_base: usize,
    mounts: MountTable,
    files: FdTable,
    cache: FileCache,
    path_owners: BTreeMap<alloc::string::String, usize>,
    read_rings: BTreeMap<usize, ReadRingSession>,
    free_ring_slots: Vec<usize>,
    clock_token: usize,
    views: view::VfsViewTable,
    client_containers: BTreeMap<usize, u64>,
    /// Per-container ephemeral in-memory filesystems (keyed by container_id).
    container_memfs: BTreeMap<u64, mount::MemFsBackend>,
    /// Per-client bounce-buffer pool for big single-shot replies (readdir
    /// blobs, etc.). Reply carries `(used_bounce, len)`; client memcpys
    /// out. One outstanding RPC per client (sync IPC), so overwrite-on-reply
    /// is safe.
    bounce_pool: bulk_pool::BulkPool,
    /// Registry of live `/dev/pts/<id>` pseudo-terminal slaves.
    /// Heap-allocated so the address is stable for the `PtsBackend` raw pointer.
    pts_registry: alloc::boxed::Box<pts::PtsRegistry>,
    dev_registry: alloc::boxed::Box<mount::DevRegistry>,
    /// Live tty:N main endpoints.  Populated lazily as registry grants arrive.
    /// Index 0 = tty:0 (VT0).  Used to satisfy read/write on /dev/tty* and
    /// /dev/tty0 when the DeviceBackend was opened before the tty service
    /// registered (i.e. endpoint was 0 at open-time).
    tty_endpoints: [usize; 4],
    /// Parked shell reads awaiting async delivery from cluuterm.
    ///
    /// Keyed by `pts_id`.  When `handle_read_grant` sees a Pts open-file it
    /// parks a `ParkedRead` here and sends a fire-and-forget drain-hint
    /// (`PTS_READ_LABEL`) to cluuterm instead of blocking.  Cluuterm replies
    /// with `PTS_READ_DELIVER_LABEL`; the handler pops the front entry, grants
    /// the payload bytes into the shell's address space, and replies the parked
    /// `reply_token` to unblock the shell.
    pending_pts_reads: BTreeMap<u32, VecDeque<ParkedRead>>,
    /// Open handles for synthesised virtual-root directories (e.g. `/`, `/dev`
    /// under a USER view).  High bit is always set so they are distinguishable
    /// from real fd_table entries at zero cost.
    virtual_fds: BTreeMap<u32, alloc::vec::Vec<alloc::string::String>>,
    /// Monotone counter for virtual fd allocation.
    virtual_fd_counter: u32,
}

impl VfsServer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        endpoint: usize,
        space_token: usize,
        vfs_space_map_token: usize,
        clock_token: usize,
        grant_buf_base: usize,
        grant_buf_size: usize,
        cache_buf_base: usize,
        cache_buf_size: usize,
        ring_pool_base: usize,
        ring_pool_size: usize,
        bounce_pool_base: usize,
        bounce_pool_size: usize,
        mut mounts: MountTable,
        session_id: Option<u32>,
    ) -> Self {
        let mut free_ring_slots = Vec::new();
        let ring_slots = ring_pool_size / RING_SLOT_BYTES;
        for slot in 0..ring_slots {
            free_ring_slots.push(slot);
        }
        free_ring_slots.reverse();

        let bounce_slots = bounce_pool_size / BOUNCE_SLOT_BYTES;
        let bounce_pool = bulk_pool::BulkPool::new(bounce_pool_base, BOUNCE_SLOT_BYTES, bounce_slots);

        // Allocate the PTS registry on the heap so its address is stable for
        // the duration of the VFS service.  The raw pointer is passed to
        // PtsBackend; it remains valid because pts_registry lives in Self.
        let pts_registry = alloc::boxed::Box::new(pts::PtsRegistry::new());
        let pts_reg_ptr: *const pts::PtsRegistry = &*pts_registry;
        let pts_backend = match session_id {
            Some(sid) => pts::PtsBackend::for_session(pts_reg_ptr, sid),
            None => pts::PtsBackend::new(pts_reg_ptr),
        };
        mounts.mount_sync(
            "/dev/pts",
            alloc::boxed::Box::new(pts_backend),
        );

        let dev_registry = alloc::boxed::Box::new(mount::DevRegistry::new());
        let dev_reg_ptr: *const mount::DevRegistry = &*dev_registry;
        let dev_reg_mount = mount::DevRegistryMount::new(dev_reg_ptr);
        mounts.mount_sync("/dev/input", alloc::boxed::Box::new(dev_reg_mount));
        let _ = debug_print("vfs: mounted /dev/input (devreg)");

        Self {
            endpoint,
            space_token,
            vfs_space_map_token,
            grant_buf_base,
            grant_buf_size,
            ring_pool_base,
            mounts,
            files: FdTable::new(),
            cache: FileCache::new(cache_buf_base, cache_buf_size),
            path_owners: BTreeMap::new(),
            read_rings: BTreeMap::new(),
            free_ring_slots,
            clock_token,
            views: view::VfsViewTable::new(),
            client_containers: BTreeMap::new(),
            container_memfs: BTreeMap::new(),
            bounce_pool,
            pts_registry,
            dev_registry,
            tty_endpoints: [0usize; 4],
            pending_pts_reads: BTreeMap::new(),
            virtual_fds: BTreeMap::new(),
            virtual_fd_counter: 0,
        }
    }

    fn clock_sample(&self) -> u64 {
        if self.clock_token == 0 {
            return 0;
        }
        clock_now(self.clock_token).unwrap_or(0)
    }

    fn log_map_elf_stage(&self, fd: usize, stage: &str, start_ts: u64) {
        const ELF_PROFILE: bool = false;
        if !ELF_PROFILE {
            return;
        }
        let now = self.clock_sample();
        let elapsed_us = now.saturating_sub(start_ts);
        let _ = debug_print(&format!(
            "vfs: map_elf[{}] {} +{}us", fd, stage, elapsed_us
        ));
    }

    fn handle_message(&mut self, msg: &Message, payload: &[u8], sender_tid: usize, runtime: &mut Runtime) -> Result<()> {
        // Control messages handled before regular VFS ops.
        if msg.tag.label == libcluu::ipc::VFS_SET_VIEW_LABEL {
            return self.handle_set_view(msg, payload, sender_tid);
        }
        if msg.tag.label == libcluu::ipc::VFS_CONTAINER_CLEANUP_LABEL {
            return self.handle_container_cleanup(msg, sender_tid);
        }
        if msg.tag.label == libcluu::fs::protocol::VFS_FLUSH {
            return self.handle_flush();
        }
        // PTS control messages.
        if msg.tag.label == PTS_REGISTER_LABEL {
            return self.handle_pts_register(msg, sender_tid);
        }
        if msg.tag.label == libcluu::ipc::VFS_REGISTER_DEV_LABEL {
            return self.handle_dev_register(msg, payload);
        }
        if msg.tag.label == libcluu::proto::pts::VFS_REGISTER_PTS_LABEL {
            return self.handle_vfs_register_pts(msg, payload, sender_tid);
        }
        if msg.tag.label == PTS_UNREGISTER_LABEL {
            return self.handle_pts_unregister(msg, sender_tid);
        }
        if msg.tag.label == VFS_DERIVE_CHILD_FD_LABEL {
            return self.handle_derive_child_fd(msg);
        }
        if msg.tag.label == libcluu::proto::pts::PTS_READ_DELIVER_LABEL {
            return self.handle_pts_read_deliver(msg, payload);
        }
        if msg.tag.label == libcluu::proto::pts::PTS_SET_PGRP_LABEL {
            return self.forward_pts_verb_to_cluuterm(
                msg,
                payload,
                sender_tid,
                libcluu::proto::pts::PTS_SET_PGRP_LABEL,
                runtime,
            );
        }
        if msg.tag.label == libcluu::proto::pts::PTS_GET_TERMIOS_LABEL {
            return self.forward_pts_verb_to_cluuterm(
                msg,
                payload,
                sender_tid,
                libcluu::proto::pts::PTS_GET_TERMIOS_LABEL,
                runtime,
            );
        }
        if msg.tag.label == libcluu::proto::pts::PTS_SET_TERMIOS_LABEL {
            return self.forward_pts_verb_to_cluuterm(
                msg,
                payload,
                sender_tid,
                libcluu::proto::pts::PTS_SET_TERMIOS_LABEL,
                runtime,
            );
        }
        if msg.tag.label == libcluu::proto::pts::PTS_GET_WINSIZE_LABEL {
            return self.forward_pts_verb_to_cluuterm(
                msg,
                payload,
                sender_tid,
                libcluu::proto::pts::PTS_GET_WINSIZE_LABEL,
                runtime,
            );
        }
        if msg.tag.label == libcluu::proto::pts::PTS_SET_WINSIZE_LABEL {
            return self.forward_pts_verb_to_cluuterm(
                msg,
                payload,
                sender_tid,
                libcluu::proto::pts::PTS_SET_WINSIZE_LABEL,
                runtime,
            );
        }
        let Some(op) = VfsOp::from_label(msg.tag.label) else {
            vfs_trace!("vfs: unknown op");
            return Ok(());
        };
        let reply_token = extract_reply_id(msg).unwrap_or(self.endpoint);
        let authenticated_client = (sender_tid != 0).then_some(sender_tid);
        vfs_trace!("vfs: handling {:?} reply_token={}", op, reply_token);

        // Async dispatch for async mounts — path-based ops only.
        let async_path: Option<&str> = match op {
            VfsOp::Open | VfsOp::Readdir | VfsOp::Stat
            | VfsOp::Unlink | VfsOp::Mkdir | VfsOp::Rmdir | VfsOp::Realpath => {
                core::str::from_utf8(payload).ok()
            }
            VfsOp::Rename | VfsOp::Link => {
                let old_len = msg.words[2];
                if old_len <= payload.len() {
                    core::str::from_utf8(&payload[..old_len]).ok()
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(path_str) = async_path {
            if self.mounts.is_async(path_str) {
                return self.dispatch_async(op, msg, payload, path_str, reply_token, authenticated_client, runtime);
            }
        }

        let result = match op {
            VfsOp::Open => self.handle_open(msg, payload, reply_token, authenticated_client),
            VfsOp::Close => self.handle_close(msg, reply_token, authenticated_client),
            VfsOp::ReadGrant => {
                self.handle_read_grant(msg, payload, reply_token, authenticated_client, runtime)
            }
            VfsOp::Readdir => self.handle_readdir(msg, payload, reply_token, authenticated_client),
            VfsOp::MapElf => self.handle_map_elf(msg, reply_token, authenticated_client),
            VfsOp::Write => self.handle_write(msg, payload, reply_token, authenticated_client),
            VfsOp::Stat => self.handle_stat(msg, payload, reply_token, authenticated_client),
            VfsOp::Fstat => self.handle_fstat(msg, reply_token, authenticated_client),
            VfsOp::Unlink => self.handle_unlink(msg, payload, reply_token, authenticated_client),
            VfsOp::Mkdir => self.handle_mkdir(msg, payload, reply_token, authenticated_client),
            VfsOp::Rmdir => self.handle_rmdir(msg, payload, reply_token, authenticated_client),
            VfsOp::Rename => self.handle_rename(msg, payload, reply_token, authenticated_client),
            VfsOp::Link => self.handle_link(msg, payload, reply_token, authenticated_client),
            VfsOp::Realpath => {
                self.handle_realpath(msg, payload, reply_token, authenticated_client)
            }
            VfsOp::RingSetup => {
                self.handle_ring_setup(msg, payload, reply_token, authenticated_client)
            }
            VfsOp::ReadRing => self.handle_read_ring(msg, reply_token, authenticated_client, runtime),
            VfsOp::BounceSetup => {
                self.handle_bounce_setup(msg, payload, reply_token, authenticated_client)
            }
        };
        vfs_trace!("vfs: handled {:?} result={:?}", op, result);
        result
    }

    fn dispatch_async(
        &mut self,
        op: VfsOp,
        msg: &Message,
        payload: &[u8],
        path: &str,
        reply_token: usize,
        caller_client: Option<usize>,
        runtime: &mut Runtime,
    ) -> Result<()> {
        let client_id = match self.resolve_client_id("async", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                let mut reply_msg = Message::new(op_reply_label(op), [0; 6], 1);
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let (_real_path, _target) = match self.view_check_path_with_target(client_id, path) {
            Ok(pt) => pt,
            Err(err) => {
                let mut reply_msg = Message::new(op_reply_label(op), [0; 6], 1);
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        // For two-path ops (Rename, Link), parse and view-check the second path.
        let rel_new_path: Option<String> = if matches!(op, VfsOp::Rename | VfsOp::Link) {
            let old_len = msg.words[2];
            if old_len > payload.len() {
                let mut reply_msg = Message::new(op_reply_label(op), [0; 6], 1);
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            let new_path = match core::str::from_utf8(&payload[old_len..]) {
                Ok(p) => p,
                Err(_) => {
                    let mut reply_msg = Message::new(op_reply_label(op), [0; 6], 1);
                    reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            };
            match self.view_check_path_with_target(client_id, new_path) {
                Ok(_) => {}
                Err(err) => {
                    let mut reply_msg = Message::new(op_reply_label(op), [0; 6], 1);
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
            Some(self.mounts.split_path(new_path).1.to_string())
        } else {
            None
        };

        let backend_ref = self.mounts.get_async_backend(path).unwrap();
        let backend_parts: [usize; 2] = unsafe {
            core::mem::transmute_copy::<&dyn mount::AsyncMountBackend, [usize; 2]>(&backend_ref)
        };

        let rel_path = self.mounts.split_path(path).1.to_string();
        let full_path = path.to_string();
        let caller_tid = caller_client.unwrap_or(0);

        match op {
            VfsOp::Open => {
                runtime.spawn(async move {
                    // SAFETY: Single-threaded VFS. VfsServer never drops.
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.open_async(&rel_path, &full_path, caller_tid).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Open { reply_token, client_id, result }
                    );
                });
            }
            VfsOp::Readdir => {
                runtime.spawn(async move {
                    // SAFETY: Single-threaded VFS. VfsServer never drops.
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.readdir_async(&rel_path, caller_tid).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Readdir { reply_token, client_id, result }
                    );
                });
            }
            VfsOp::Stat => {
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.stat_async(&rel_path, &full_path, caller_tid).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Stat { reply_token, client_id, result }
                    );
                });
            }
            VfsOp::Unlink => {
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.unlink_async(&rel_path).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Unlink { reply_token, result }
                    );
                });
            }
            VfsOp::Mkdir => {
                let mode = msg.words[2];
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.mkdir_async(&rel_path, mode).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Mkdir { reply_token, result }
                    );
                });
            }
            VfsOp::Rmdir => {
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.rmdir_async(&rel_path).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Rmdir { reply_token, result }
                    );
                });
            }
            VfsOp::Rename => {
                let rel_new = rel_new_path.unwrap_or_default();
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.rename_async(&rel_path, &rel_new).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Rename { reply_token, result }
                    );
                });
            }
            VfsOp::Link => {
                let rel_new = rel_new_path.unwrap_or_default();
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.link_async(&rel_path, &rel_new).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Link { reply_token, result }
                    );
                });
            }
            VfsOp::Realpath => {
                runtime.spawn(async move {
                    let backend: &'static dyn mount::AsyncMountBackend = unsafe {
                        core::mem::transmute_copy::<[usize; 2], &'static dyn mount::AsyncMountBackend>(&backend_parts)
                    };
                    let result = backend.realpath_async(&rel_path).await;
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::Realpath { reply_token, result }
                    );
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn complete_async_open(&mut self, reply_token: usize, client_id: usize, result: Result<OpenFile>) {
        let mut reply_msg = Message::new(VFS_OPEN, [0; 6], 3);
        match result {
            Ok(file) => {
                let size = file.size();
                let fd = self.files.open(client_id, file);
                reply_msg.words[0] = 0;
                reply_msg.words[1] = fd;
                reply_msg.words[2] = size;
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
    }

    fn complete_async_readdir(&mut self, reply_token: usize, _client_id: usize, result: Result<Vec<DirEntry>>) {
        const INLINE_BUDGET: usize = 3584;
        let mut reply_msg = Message::new(VFS_READDIR, [0; 6], 4);
        match result {
            Ok(entries) => {
                let count = entries.len();
                let mut blob = Vec::new();
                for entry in &entries {
                    let name_bytes = entry.name.as_bytes();
                    blob.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
                    blob.extend_from_slice(&entry_to_stat_bytes(entry));
                    blob.extend_from_slice(name_bytes);
                }
                if blob.len() <= INLINE_BUDGET {
                    reply_msg.words[0] = blob.len();
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = count;
                    reply_msg.words[3] = 0;
                    let _ = reply_with_payload(reply_token, &reply_msg, &blob);
                } else {
                    reply_msg.words[1] = Error::BufferTooSmall.to_errno() as usize;
                    let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
            Err(err) => {
                reply_msg.words[1] = err.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
    }

    fn complete_async_stat(&mut self, reply_token: usize, result: Result<DirEntryStat>) {
        let mut reply_msg = Message::new(VFS_STAT, [0; 6], 1);
        match result {
            Ok(stat) => {
                let info = StatInfo {
                    size: stat.size,
                    mode: stat.mode,
                    mtime: stat.mtime,
                    nlink: stat.nlink,
                    uid: stat.uid,
                    gid: stat.gid,
                    blocks: stat.blocks,
                };
                reply_msg.words[0] = 0;
                let _ = reply_with_payload(reply_token, &reply_msg, &info.to_bytes());
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
    }

    fn complete_async_read(&mut self, reply_token: usize, result: Result<Vec<u8>>) {
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        match result {
            Ok(data) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = data.len();
                reply_msg.words[2] = 0;
                let _ = reply_with_payload(reply_token, &reply_msg, &data);
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
    }

    fn complete_async_write(&mut self, reply_token: usize, result: Result<usize>) {
        let mut reply_msg = Message::new(VFS_WRITE, [0; 6], 2);
        match result {
            Ok(written) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = written;
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
    }

    fn complete_async_simple(&mut self, reply_token: usize, label: u32, result: Result<()>) {
        let mut reply_msg = Message::new(label, [0; 6], 1);
        match result {
            Ok(()) => reply_msg.words[0] = 0,
            Err(err) => reply_msg.words[0] = err.to_errno() as usize,
        }
        let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
    }

    fn complete_async_realpath(&mut self, reply_token: usize, result: Result<String>) {
        let mut reply_msg = Message::new(VFS_REALPATH, [0; 6], 2);
        match result {
            Ok(path) => {
                let bytes = path.into_bytes();
                reply_msg.words[1] = bytes.len();
                let _ = reply_with_payload(reply_token, &reply_msg, &bytes);
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
    }

    fn complete_tty_read_grant(
        &mut self,
        reply_token: usize,
        target_base: usize,
        target_space: usize,
        result: Result<Vec<u8>>,
    ) {
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        match result {
            Ok(data) => {
                let _ = self.grant_data_to_caller(&data, target_base, target_space, &mut reply_msg);
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
    }

    fn complete_tty_read_ring(
        &mut self,
        reply_token: usize,
        client_id: usize,
        max_fill: usize,
        result: Result<Vec<u8>>,
    ) {
        let mut reply_msg = Message::new(VFS_READ_RING, [0; 6], 4);
        match result {
            Ok(data) => {
                let Some(session) = self.read_rings.get(&client_id).copied() else {
                    reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
                    let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    return;
                };
                let backing = unsafe {
                    core::slice::from_raw_parts_mut(session.source_base as *mut u8, session.bytes)
                };
                let mut ring = match SharedRing::attach(backing) {
                    Ok(r) => r,
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                        let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                        return;
                    }
                };
                let pushed = ring.push(&data);
                let notify_seq = if pushed > 0 {
                    ring.bump_notify_seq()
                } else {
                    ring.notify_seq()
                };
                let eof = data.len() < max_fill;
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pushed;
                reply_msg.words[2] = notify_seq as usize;
                reply_msg.words[3] = if eof { 1 } else { 0 };
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
    }

    fn complete_pts_verb(
        &mut self,
        reply_token: usize,
        label: u32,
        result: Result<Vec<u8>>,
    ) {
        match result {
            Ok(raw_bytes) => {
                let _ = libcluu::syscall::ipc_reply(reply_token, &raw_bytes);
            }
            Err(err) => {
                let mut reply_msg = Message::new(label, [0; 6], 1);
                reply_msg.words[0] = err.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
    }

    fn handle_registry_message(&mut self, msg: &Message, payload: &[u8], _sender_tid: usize) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            if let registry::RegistryEvent::Grant { service_name, name, token } = event {
                // Populate /dev/tty* endpoints when tty:N/main grants arrive.
                if name == "main" {
                    if let Some(idx) = service_name
                        .strip_prefix("tty:")
                        .and_then(|s| s.parse::<usize>().ok())
                    {
                        if idx < 4 {
                            self.tty_endpoints[idx] = token;
                            let _ = debug_print(&format!(
                                "vfs: tty:{} main endpoint={}", idx, token
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Handle VFS_SET_VIEW_LABEL: register a per-client filesystem view.
    fn handle_set_view(&mut self, msg: &Message, payload: &[u8], sender_tid: usize) -> Result<()> {
        if sender_tid == 0 {
            let _ = debug_print("vfs: set_view denied unauthenticated sender");
            return Err(Error::PermissionDenied);
        }
        let requested_client_id = msg.words[1];
        let mount_count = msg.words[2];
        let profile_bits = if msg.tag.words >= 4 {
            msg.words[3] as u16
        } else {
            0
        };
        let profile = libcluu::cap::CapProfile::from_bits_truncate(profile_bits);
        // container_id from procmgr (0 = no container / SUPERVISOR).
        let container_id: u64 = if msg.tag.words >= 5 {
            msg.words[4] as u64
        } else {
            0
        };
        let cap_handle = if msg.tag.words as usize >= VIEW_MGR_CAP_WORD + 1 {
            msg.words[VIEW_MGR_CAP_WORD]
        } else {
            0
        };
        match resolve_view_mgr_cap(cap_handle) {
            Ok((scope_sid, scope_mask)) => {
                if scope_sid != 0 && scope_mask == 0 {
                    let _ = debug_print("vfs: set_view denied cap has empty scope_mask");
                    return Err(Error::PermissionDenied);
                }
            }
            Err(_) => {
                let _ = debug_print("vfs: set_view denied — no valid view-mgr cap");
                return Err(Error::PermissionDenied);
            }
        }

        let client_id = if requested_client_id == 0 {
            sender_tid
        } else {
            requested_client_id
        };
        if mount_count == 0 {
            self.client_containers.remove(&client_id);
            if profile_bits == 0 {
                // Process is exiting (procmgr signals this via empty view).
                // Close all the client's open fds so PTS ref-counts drop;
                // when the last consumer of a pts exits, pts_on_close fires
                // PTS_CLOSED to the cluuterm owner so the window can close.
                // Without this, PTS_CLOSED never fires for shells spawned
                // via session-procmgr (no container_cleanup path).
                let closed = self.files.close_all_for_client(client_id);
                for file in closed {
                    if let OpenFile::Pts(p) = file {
                        self.pts_on_close(p.pts_id);
                    }
                }
                self.views.remove_view(client_id);
                let _ = debug_print(&format!("vfs: set_view cleared client={}", client_id));
            } else {
                self.views.set_profile(client_id, profile);
                self.views.clear_explicit_view(client_id);
                let _ = debug_print(&format!(
                    "vfs: set_view fallback profile-only client={} profile={:#x}",
                    client_id, profile_bits
                ));
            }
            return Ok(());
        }

        let mut mounts = alloc::vec::Vec::new();
        let mut offset = 0;

        for _ in 0..mount_count {
            // Per-mount wire: u16 src_len | u16 dst_len | u8 flags | u64 memfs_cid | src | dst
            // Header: 2 + 2 + 1 + 8 = 13 bytes.
            if offset + 13 > payload.len() {
                return Err(Error::InvalidArgument);
            }
            let src_len = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as usize;
            let dst_len = u16::from_le_bytes([payload[offset + 2], payload[offset + 3]]) as usize;
            let flags = payload[offset + 4];
            let memfs_cid = u64::from_le_bytes([
                payload[offset + 5],
                payload[offset + 6],
                payload[offset + 7],
                payload[offset + 8],
                payload[offset + 9],
                payload[offset + 10],
                payload[offset + 11],
                payload[offset + 12],
            ]);
            offset += 13;

            if offset + src_len + dst_len > payload.len() {
                return Err(Error::InvalidArgument);
            }
            let src: alloc::string::String =
                core::str::from_utf8(&payload[offset..offset + src_len])
                    .map_err(|_| Error::InvalidArgument)?
                    .into();
            offset += src_len;
            let dst: alloc::string::String =
                core::str::from_utf8(&payload[offset..offset + dst_len])
                    .map_err(|_| Error::InvalidArgument)?
                    .into();
            offset += dst_len;
            view::validate_clean_absolute_path(src.as_str())?;
            view::validate_clean_absolute_path(dst.as_str())?;

            let target = if memfs_cid == 0 {
                view::MountTarget::MountTable
            } else {
                // Lazily allocate the MemFs for this container on first sight.
                if !self.container_memfs.contains_key(&memfs_cid) {
                    let memfs = mount::MemFsBackend::new(DEFAULT_MEMFS_QUOTA);
                    {
                        let mut fs = memfs.borrow_mut();
                        let _ = fs.mkdir("/tmp");
                        let _ = fs.mkdir("/log");
                    }
                    self.container_memfs.insert(memfs_cid, memfs);
                }
                view::MountTarget::MemFs { container_id: memfs_cid }
            };

            mounts.push(view::ViewMount {
                src,
                dst,
                writable: (flags & 1) != 0,
                target,
            });
        }
        if offset != payload.len() {
            return Err(Error::InvalidArgument);
        }

        // Record container membership for later cleanup/ringio paths.
        if container_id > 0 {
            self.client_containers.insert(client_id, container_id);
        }
        // NOTE: /tmp, /log, /data, and the `/ → MemFs` catch-all are now
        // procmgr's responsibility. procmgr sends explicit mount entries
        // with the correct memfs_cid per mount (see mount-policy design
        // spec). VFS just serves whatever mount list it's given.

        self.views.set_view(client_id, view::VfsView { mounts });
        if profile_bits != 0 {
            self.views.set_profile(client_id, profile);
        }
        Ok(())
    }

    /// Handle VFS_CONTAINER_CLEANUP_LABEL: clean up container storage on exit or destroy.
    fn handle_flush(&self) -> Result<()> {
        let _ = debug_print("vfs: flush requested (ext2 writes are synchronous, no-op)");
        Ok(())
    }

    fn handle_dev_register(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let device_kind = msg.words[0] as u8;
        let driver_endpoint = msg.words[1];
        let path_len = msg.words[2].min(payload.len());
        let path = core::str::from_utf8(&payload[..path_len])
            .unwrap_or("")
            .trim_end_matches('\0');

        if path.is_empty() {
            return Ok(());
        }

        self.dev_registry.register(mount::DevRegistryEntry {
            device_id: device_kind as u32,
            class: device_kind,
            driver_endpoint,
            path: alloc::string::String::from(path),
        });

        let _ = libcluu::debug_print(&alloc::format!(
            "vfs: registered dev {} (kind={}, ep={})",
            path, device_kind, driver_endpoint
        ));

        Ok(())
    }

    // ── PTS handlers ─────────────────────────────────────────────────────────

    /// Handle PTS_REGISTER_LABEL: allocate a new `/dev/pts/<id>` slot.
    ///
    /// Wire format:
    ///   words[0] = notify_endpoint (usize): VFS sends PTS_CLOSED_LABEL here
    ///              when the last fd on the pts is closed.
    ///
    /// Reply (via reply_token embedded in the message):
    ///   words[0] = errno  (0 = ok, non-zero = libcluu errno)
    ///   words[1] = id     (u32, valid only when errno == 0)
    fn handle_pts_register(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg).unwrap_or(self.endpoint);
        let mut reply_msg = Message::new(PTS_REGISTER_LABEL, [0; 6], 2);

        if sender_tid == 0 {
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let notify_endpoint = msg.words[0];

        match self.pts_registry.register(sender_tid, notify_endpoint) {
            Some(id) => {
                let _ = debug_print(&format!(
                    "vfs: pts_register owner_tid={} id={}", sender_tid, id
                ));
                reply_msg.words[0] = 0;
                reply_msg.words[1] = id as usize;
            }
            None => {
                let _ = debug_print("vfs: pts_register pool exhausted");
                reply_msg.words[0] = Error::OutOfMemory.to_errno() as usize;
            }
        }

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    /// Handle VFS_REGISTER_PTS_LABEL (111): allocate a new `/dev/pts/<id>` slot
    /// with optional `session_id` for per-session overlay.
    ///
    /// Wire format (payload = postcard-serialized VfsRegisterPtsRequest):
    ///   session_id:   Option<u32>
    ///   pts_endpoint: u64
    ///   suggested_id: Option<u32>
    ///
    /// Reply (payload = postcard-serialized VfsRegisterPtsReply):
    ///   assigned_id: u32
    ///
    /// When `session_id` is `None`, the entry lands in the global namespace
    /// (identical to the legacy PTS_REGISTER_LABEL path).  When `Some`, the
    /// entry lands in `by_session[sid]` and is only visible in that session's
    /// derived `/dev/pts/` overlay.
    fn handle_vfs_register_pts(
        &mut self,
        msg: &Message,
        payload: &[u8],
        sender_tid: usize,
    ) -> Result<()> {
        let reply_token = extract_reply_id(msg).unwrap_or(self.endpoint);

        if sender_tid == 0 {
            let reply_bytes = postcard::to_allocvec(&Error::PermissionDenied.to_errno())
                .unwrap_or_default();
            let reply_msg = Message::new(
                libcluu::proto::pts::VFS_REGISTER_PTS_LABEL,
                [Error::PermissionDenied.to_errno() as usize, 0, 0, 0, 0, 0],
                1,
            );
            return ipc::reply_with_payload(reply_token, &reply_msg, &reply_bytes);
        }

        let req: libcluu::proto::pts::VfsRegisterPtsRequest =
            match postcard::from_bytes(payload) {
                Ok(r) => r,
                Err(_) => {
                    let reply_bytes = postcard::to_allocvec(&Error::InvalidArgument.to_errno())
                        .unwrap_or_default();
                    let reply_msg = Message::new(
                        libcluu::proto::pts::VFS_REGISTER_PTS_LABEL,
                        [Error::InvalidArgument.to_errno() as usize, 0, 0, 0, 0, 0],
                        1,
                    );
                    return ipc::reply_with_payload(reply_token, &reply_msg, &reply_bytes);
                }
            };

        let notify_endpoint = req.pts_endpoint as usize;

        match self.pts_registry.register_in_session(
            req.session_id,
            sender_tid,
            notify_endpoint,
        ) {
            Some(assigned_id) => {
                let _ = debug_print(&format!(
                    "vfs: vfs_register_pts session={:?} owner_tid={} id={}",
                    req.session_id, sender_tid, assigned_id
                ));
                let reply = libcluu::proto::pts::VfsRegisterPtsReply { assigned_id };
                let reply_bytes =
                    postcard::to_allocvec(&reply).unwrap_or_default();
                let reply_msg = Message::new(
                    libcluu::proto::pts::VFS_REGISTER_PTS_LABEL,
                    [0, assigned_id as usize, 0, 0, 0, 0],
                    2,
                );
                ipc::reply_with_payload(reply_token, &reply_msg, &reply_bytes)
            }
            None => {
                let _ = debug_print("vfs: vfs_register_pts pool exhausted");
                let reply_bytes = postcard::to_allocvec(&Error::OutOfMemory.to_errno())
                    .unwrap_or_default();
                let reply_msg = Message::new(
                    libcluu::proto::pts::VFS_REGISTER_PTS_LABEL,
                    [Error::OutOfMemory.to_errno() as usize, 0, 0, 0, 0, 0],
                    1,
                );
                ipc::reply_with_payload(reply_token, &reply_msg, &reply_bytes)
            }
        }
    }

    /// Handle PTS_UNREGISTER_LABEL: release a `/dev/pts/<id>` slot.
    ///
    /// Wire format:
    ///   words[0] = id (u32)
    ///
    /// Only the original registrant (matched by sender_tid) may unregister.
    ///
    /// Reply:
    ///   words[0] = errno (0 = ok)
    fn handle_pts_unregister(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg).unwrap_or(self.endpoint);
        let mut reply_msg = Message::new(PTS_UNREGISTER_LABEL, [0; 6], 1);

        let id = msg.words[0] as u32;

        // Ownership check: only the registrant may unregister.
        match self.pts_registry.owner_tid(id) {
            None => {
                // Not found — treat as success (idempotent).
                reply_msg.words[0] = 0;
            }
            Some(owner) if sender_tid != 0 && owner != sender_tid => {
                let _ = debug_print(&format!(
                    "vfs: pts_unregister denied id={} owner={} caller={}",
                    id, owner, sender_tid
                ));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            _ => {
                self.pts_registry.unregister(id);
                let _ = debug_print(&format!("vfs: pts_unregister id={}", id));
                reply_msg.words[0] = 0;
            }
        }

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    /// Handle `PTS_READ_DELIVER_LABEL` (112) — cluuterm pushes cooked bytes for a
    /// parked shell read.
    ///
    /// Wire layout (fire-and-forget from cluuterm, no reply slot):
    ///   `words[0]` = payload_len (written by `send_msg_with_payload`)
    ///   `words[1]` = pts_id
    ///   payload    = raw cooked bytes
    ///
    /// Pops the front `ParkedRead` for `pts_id`, grants the payload bytes into the
    /// shell's address space (`caller_space`/`target_base`), then replies the
    /// parked `reply_token` to unblock the shell's `read(2)`.
    fn handle_pts_read_deliver(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let pts_id = msg.words[1] as u32;
        let eof = msg.words[2] != 0;

        let parked = match self
            .pending_pts_reads
            .get_mut(&pts_id)
            .and_then(|q| q.pop_front())
        {
            Some(p) => p,
            None => {
                let _ = debug_print(&format!(
                    "vfs: pts_read_deliver pts_id={} — no parked read (stale delivery?)",
                    pts_id
                ));
                return Ok(());
            }
        };

        let data: Vec<u8> = payload.iter().copied().take(parked.requested).collect();

        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);

        if data.is_empty() && !eof {
            // Cluuterm sent zero bytes without EOF — no cooked bytes available
            // yet. Re-park; cluuterm will send another PTS_READ_DELIVER when
            // bytes arrive (via DeliverBytes path).
            self.pending_pts_reads
                .entry(pts_id)
                .or_insert_with(VecDeque::new)
                .push_front(parked);
            return Ok(());
        }

        if data.is_empty() {
            // EOF: reply with 0-byte grant so the parked reader unblocks
            // with read() == 0.
            reply_msg.words[0] = 0;
            let _ = debug_print(&format!(
                "vfs: pts_read_deliver pts_id={} EOF (0 bytes)",
                pts_id
            ));
            return reply_parked(&parked, &reply_msg, &[]);
        }

        self.grant_data_to_caller(
            &data,
            parked.target_base,
            parked.caller_space,
            &mut reply_msg,
        )?;

        let _ = debug_print(&format!(
            "vfs: pts_read_deliver pts_id={} granted {} bytes",
            pts_id,
            data.len()
        ));

        reply_parked(&parked, &reply_msg, &[])
    }

    /// Proxy a PTS_* verb from a VFS client to the owning cluuterm instance.
    ///
    /// Used for `PTS_SET_PGRP_LABEL` (138), `PTS_GET_TERMIOS_LABEL` (133),
    /// and `PTS_SET_TERMIOS_LABEL` (134).  Cluuterm owns the canonical PTS
    /// state (termios, fg pgid, winsize); VFS just forwards the call so that
    /// VFS-backed fds (e.g. a shell's stdin)
    /// can reach cluuterm without a separate endpoint.
    ///
    /// Wire layout from libcluu `pts_call_raw`:
    ///   `words[0]` = payload_len (parse_message convention)
    ///   `words[1]` = vfs_remote_fd (the VFS-side fd handle for this client)
    ///   payload    = postcard-encoded request (verb-specific)
    ///
    /// This handler:
    ///   1. Resolves `sender_tid` → `client_id` (the authenticated client).
    ///   2. Looks up `vfs_remote_fd` in `self.files` → `OpenFile::Pts`.
    ///   3. Gets the cluuterm notify endpoint from `pts_registry`.
    ///   4. Calls cluuterm synchronously with `label` + payload.
    ///   5. Forwards cluuterm's reply payload back to the original caller.
    fn forward_pts_verb_to_cluuterm(
        &mut self,
        msg: &Message,
        payload: &[u8],
        sender_tid: usize,
        label: u32,
        runtime: &mut Runtime,
    ) -> Result<()> {
        let reply_token = match ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => {
                // Fire-and-forget (no reply slot) — nothing to do.
                return Ok(());
            }
        };

        let mut reply_msg = Message::new(label, [0; 6], 1);

        // Authenticate caller and resolve client_id from sender TID.
        let client_id = match (sender_tid != 0).then_some(sender_tid) {
            Some(id) => id,
            None => {
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        // `words[1]` carries the VFS-side remote_fd from `pts_call_raw`.
        let vfs_fd = msg.words[1];

        // Resolve the open file.
        let pts_id = match self.files.get(client_id, vfs_fd) {
            Some(OpenFile::Pts(p)) => p.pts_id,
            Some(_) => {
                // fd exists but is not a PTS — ENOTTY.
                reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            None => {
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        // Get the cluuterm notify endpoint.
        let cluuterm_ep = match self.pts_registry.notify_endpoint(pts_id) {
            Some(ep) if ep != 0 => ep,
            _ => {
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        // Async forward: spawn IpcCallFuture to avoid blocking the recv loop.
        // The completion handler reconstructs the raw reply (header + payload)
        // and forwards it verbatim to the original caller via ipc_reply.
        let payload_vec = payload.to_vec();
        let pts_id_usize = pts_id as usize;
        runtime.spawn(async move {
            let mut fwd_req = Message::new(
                label,
                [payload_vec.len(), pts_id_usize, 0, 0, 0, 0],
                2,
            );
            let result = IpcCallFuture::new_with_payload(cluuterm_ep, &mut fwd_req, &payload_vec).await;
            let completion = match result {
                Ok((reply_msg, reply_payload)) => {
                    let mut raw = Vec::with_capacity(
                        core::mem::size_of::<Message>() + reply_payload.len(),
                    );
                    raw.extend_from_slice(reply_msg.as_bytes());
                    raw.extend_from_slice(&reply_payload);
                    Ok(raw)
                }
                Err(e) => Err(e),
            };
            libcluu::async_runtime::push_completion(
                VfsCompletion::PtsVerb {
                    reply_token,
                    label,
                    result: completion,
                },
            );
        });
        Ok(())
    }

    /// Handle `VFS_DERIVE_CHILD_FD_LABEL` — clone a parent's open file to a
    /// child client_id and mint a narrowed VFS token from VFS's own full-rights
    /// endpoint.
    ///
    /// Called by procmgr's FdInherit handler during `posix_spawn` when the parent
    /// has a VFS-backed fd (pts, ext2, memfs) in its file-action list.
    ///
    /// Wire format (see `libcluu::ipc::VFS_DERIVE_CHILD_FD_LABEL`):
    ///
    /// Request:
    ///   words[0] = parent_client_id
    ///   words[1] = parent_remote_fd
    ///   words[2] = child_rights   (rights bits to narrow to)
    ///   words[3] = child_tid      (becomes the child's client_id)
    ///
    /// Reply:
    ///   words[0] = status (0 or errno)
    ///   words[1] = derived token handle
    ///   words[2] = child_client_id (echo of child_tid)
    ///   words[3] = child_remote_fd (freshly allocated under child_client_id)
    fn handle_derive_child_fd(&mut self, msg: &Message) -> Result<()> {
        let mut reply_msg = Message::new(VFS_DERIVE_CHILD_FD_LABEL, [0; 6], 4);

        let parent_cid   = msg.words[0];
        let parent_fd    = msg.words[1];
        let raw_rights   = msg.words[2] as u64;
        let child_tid    = msg.words[3];

        let (cloned, parent_rights): (OpenFile, u64) = match self.files.get(parent_cid, parent_fd) {
            Some(f) => (f.clone(), f.rights()),
            None => {
                let _ = debug_print(&alloc::format!(
                    "vfs: derive_child_fd lookup miss parent_cid={} parent_fd={}",
                    parent_cid, parent_fd
                ));
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
            }
        };

        let clamped_rights = raw_rights & parent_rights;
        if clamped_rights != raw_rights {
            let _ = debug_print(&format!(
                "vfs: derive_child_fd clamp parent_cid={} parent_fd={} \
                 raw=0x{:x} parent=0x{:x} clamped=0x{:x}",
                parent_cid, parent_fd, raw_rights, parent_rights, clamped_rights
            ));
        }

        if let OpenFile::Pts(ref pts) = cloned {
            let _ = self.pts_registry.inc_ref(pts.pts_id);

            // Evict any pending reads on this PTS. When a child fd is
            // derived from a PTS fd, the parent (shell) may have a stale
            // async read parked in the queue from its stdin reader loop.
            // Without this drain, the parent's read would consume bytes
            // meant for the foreground child (FIFO delivery, first-parked
            // wins).  Reply WouldBlock so the caller re-arms.
            if let Some(queue) = self.pending_pts_reads.get_mut(&pts.pts_id) {
                let mut cancel_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
                cancel_msg.words[0] = Error::WouldBlock.to_errno() as usize;
                while let Some(parked) = queue.pop_front() {
                    if parked.reply_ep != 0 {
                        let mut msg = cancel_msg.clone();
                        msg.words[libcluu::ipc::ASYNC_REPLY_COOKIE_WORD] = parked.cookie;
                        let _ = ipc::send(parked.reply_ep, &msg, IpcFlags::empty());
                    } else {
                        let _ = ipc::reply(parked.reply_token, &cancel_msg, IpcFlags::empty());
                    }
                }
            }
        }

        let child_fd = self.files.open(child_tid, cloned);
        if let Some(entry) = self.files.get_mut(child_tid, child_fd) {
            entry.set_rights(clamped_rights);
        }

        let derived = match token_derive(self.endpoint, clamped_rights as usize, u64::MAX) {
            Ok(t) => t,
            Err(e) => {
                self.files.close(child_tid, child_fd);
                let _ = debug_print(&format!(
                    "vfs: derive_child_fd token_derive failed {:?}", e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
                return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
            }
        };

        let _ = debug_print(&format!(
            "vfs: derive_child_fd parent_cid={} parent_fd={} child_tid={} child_fd={} rights=0x{:x}",
            parent_cid, parent_fd, child_tid, child_fd, clamped_rights
        ));

        reply_msg.words[0] = 0;
        reply_msg.words[1] = derived;
        reply_msg.words[2] = child_tid;
        reply_msg.words[3] = child_fd;
        ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty())
    }

    /// Called by handle_open after a successful open of `/dev/pts/<id>`.
    /// Increments the refcount for the pts id.
    fn pts_on_open(&mut self, id: u32) {
        let _ = self.pts_registry.inc_ref(id);
    }

    /// Called by handle_close when the closed fd is a Pts file.
    /// Decrements the refcount; if it reaches zero, fires PTS_CLOSED_LABEL to
    /// the owner's notify_endpoint.
    fn pts_on_close(&mut self, id: u32) {
        if let Some((new_refcount, notify_endpoint)) = self.pts_registry.dec_ref(id) {
            if new_refcount == 0 && notify_endpoint != 0 {
                // Fire-and-forget: owner is notified that all fds are closed.
                let msg = Message::new(
                    libcluu::proto::pts::PTS_CLOSED_LABEL,
                    [id as usize, 0, 0, 0, 0, 0],
                    1,
                );
                let _ = ipc::send(notify_endpoint, &msg, IpcFlags::empty());
                let _ = debug_print(&format!("vfs: pts_closed id={}", id));
            }
        }
    }

    fn handle_container_cleanup(&mut self, msg: &Message, _sender_tid: usize) -> Result<()> {
        let cap_handle = if msg.tag.words as usize >= VIEW_MGR_CAP_WORD + 1 {
            msg.words[VIEW_MGR_CAP_WORD]
        } else {
            0
        };
        match resolve_view_mgr_cap(cap_handle) {
            Ok((scope_sid, scope_mask)) => {
                if scope_sid != 0 && scope_mask == 0 {
                    let _ = debug_print("vfs: container_cleanup denied cap has empty scope_mask");
                    return Err(Error::PermissionDenied);
                }
            }
            Err(_) => {
                let _ = debug_print("vfs: container_cleanup denied — no valid view-mgr cap");
                return Err(Error::PermissionDenied);
            }
        }

        let container_id = msg.words[1] as u64;
        let mode = msg.words[2];

        // DIAG(unmap-bug): log cleanup with cache stats so ordering vs map_elf is visible
        let _ = debug_print(&format!(
            "vfs: container_cleanup START c-{} mode={} cache_entries={}",
            container_id, mode, self.cache.entries.len()
        ));

        if container_id == 0 {
            return Ok(());
        }

        let base = format!("/var/containers/c-{}", container_id);

        match mode {
            0 => {
                // EXIT: reset the MemFs (drop all inodes, re-create /tmp and /log).
                // /tmp and /log are MemFs-backed — no ext2 cleanup needed for them.
                if let Some(memfs) = self.container_memfs.get(&container_id) {
                    let mut fs = memfs.borrow_mut();
                    fs.drop_all();
                    let _ = fs.mkdir("/tmp");
                    let _ = fs.mkdir("/log");
                }
                let _ = debug_print(&format!(
                    "vfs: container cleanup exit c-{}",
                    container_id
                ));
            }
            1 => {
                // DESTROY: remove the MemFs entirely + delete persistent ext2 /data dir.
                self.container_memfs.remove(&container_id);
                self.recursive_delete(&format!("{}/data", base));
                let _ = self.mounts.rmdir(&format!("{}/data", base));
                let _ = self.mounts.rmdir(&base);
                let _ = debug_print(&format!(
                    "vfs: container cleanup destroy c-{}",
                    container_id
                ));
            }
            _ => {
                let _ = debug_print(&format!(
                    "vfs: container_cleanup unknown mode={}",
                    mode
                ));
            }
        }

        // Clear client_containers entries for this container_id
        self.client_containers
            .retain(|_, &mut cid| cid != container_id);

        Ok(())
    }

    /// Recursively delete all contents of a directory (files and subdirs).
    /// The directory itself is NOT removed — only its contents.
    /// Operates on real backing paths (no view translation).
    /// Silently ignores errors (best-effort cleanup).
    fn recursive_delete(&mut self, dir_path: &str) {
        // Defense-in-depth: only allow deletion under container storage prefix.
        if !dir_path.starts_with("/var/containers/c-") {
            let _ = debug_print(&format!(
                "vfs: recursive_delete blocked non-container path: {}",
                dir_path
            ));
            return;
        }
        let entries = match self.mounts.readdir(dir_path, 0) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let child_path = format!("{}/{}", dir_path, entry.name);
            if entry.is_dir {
                self.recursive_delete(&child_path);
                let _ = self.mounts.rmdir(&child_path);
            } else {
                let _ = self.mounts.unlink(&child_path);
            }
        }

        self.invalidate_cache_after_mutation();
    }

    /// Invalidate the file cache after a path-mutating operation (unlink,
    /// mkdir, rmdir, rename, recursive_delete).
    ///
    /// FIXME(MAP_SHARE_PHYS): currently a no-op. Resetting CacheRegion
    /// bookkeeping while sharer processes (e.g. console loaded via
    /// MAP_SHARE_PHYS at boot) still hold PTEs into cache phys frames lets a
    /// later cache_fill overwrite live text/.rodata of the sharers, causing
    /// wild-jump faults. Revisit when refcount-aware invalidation lands.
    fn invalidate_cache_after_mutation(&mut self) {
        let _ = self;
    }

    /// Check a path against the client's VFS view, rewriting if needed.
    /// Returns the real backing path, or an error if disallowed.
    #[allow(dead_code)]
    fn view_check_path(&self, client_id: usize, path: &str) -> Result<alloc::string::String> {
        self.views.check_path(client_id, path)
    }

    /// Like view_check_path, but also enforces the writable flag.
    /// Returns Error::PermissionDenied if the matching mount is read-only.
    #[allow(dead_code)]
    fn view_check_path_writable(
        &self,
        client_id: usize,
        path: &str,
    ) -> Result<alloc::string::String> {
        let (real_path, writable) = self.views.check_path_writable(client_id, path)?;
        if !writable {
            return Err(Error::PermissionDenied);
        }
        Ok(real_path)
    }

    /// Check a path and return the resolved path + mount target.
    fn view_check_path_with_target(
        &self,
        client_id: usize,
        path: &str,
    ) -> Result<(alloc::string::String, view::MountTarget)> {
        self.views.check_path_with_target(client_id, path)
    }

    /// Like view_check_path_writable, but also returns the MountTarget.
    fn view_check_path_writable_with_target(
        &self,
        client_id: usize,
        path: &str,
    ) -> Result<(alloc::string::String, view::MountTarget)> {
        let (real_path, writable, target) =
            self.views.check_path_writable_with_target(client_id, path)?;
        if !writable {
            return Err(Error::PermissionDenied);
        }
        Ok((real_path, target))
    }

    /// Get the MemFsBackend for a container.
    fn get_container_memfs(&self, container_id: u64) -> Result<&mount::MemFsBackend> {
        self.container_memfs
            .get(&container_id)
            .ok_or(Error::NotFound)
    }

    /// Stat a path on a container's MemFs. Returns full StatInfo.
    fn stat_memfs_path(&self, container_id: u64, path: &str) -> Result<StatInfo> {
        let memfs_backend = self.get_container_memfs(container_id)?;
        let fs = memfs_backend.borrow();
        if let Ok((_, size)) = fs.open(path) {
            let sz = size as u64;
            return Ok(StatInfo {
                size: sz,
                mode: MODE_FILE as u32,
                mtime: 0,
                nlink: 1,
                uid: 0,
                gid: 0,
                blocks: (sz + 511) / 512,
            });
        }
        if fs.readdir(path).is_ok() {
            return Ok(StatInfo {
                size: 0,
                mode: MODE_DIR as u32,
                mtime: 0,
                nlink: 1,
                uid: 0,
                gid: 0,
                blocks: 0,
            });
        }
        Err(Error::NotFound)
    }

    fn resolve_client_id(
        &self,
        op_name: &str,
        caller_client: Option<usize>,
        claimed_client: usize,
    ) -> Result<usize> {
        let Some(client_id) = caller_client else {
            let _ = debug_print(&format!(
                "vfs: {} denied missing authenticated sender",
                op_name
            ));
            return Err(Error::PermissionDenied);
        };
        // claimed_client mismatch is normal — auth uses sender_tid, not the
        // payload field.  We accept the authenticated client_id silently;
        // logging every mismatch was the dominant log noise during fast
        // VFS workloads.
        let _ = claimed_client; // intentionally unused
        Ok(client_id)
    }

    /// Allocate a new virtual-directory fd id.  High bit marks it as virtual
    /// so close/readdir can branch without a separate lookup.
    fn alloc_virtual_fd(
        &mut self,
        entries: alloc::vec::Vec<alloc::string::String>,
    ) -> u32 {
        let id = 0x8000_0000u32 | (self.virtual_fd_counter & 0x7FFF_FFFF);
        self.virtual_fd_counter = self.virtual_fd_counter.wrapping_add(1);
        self.virtual_fds.insert(id, entries);
        id
    }

    fn handle_open(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_OPEN, [0; 6], 3);
        let client_id = match self.resolve_client_id("open", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let path = match core::str::from_utf8(payload) {
            Ok(path) => path,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let flags = msg.words[2];
        let mode = msg.words[3];

        let write_capable_open = (flags & (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC)) != 0;
        let (real_path, target) = if write_capable_open {
            match self.view_check_path_writable_with_target(client_id, path) {
                Ok(pt) => pt,
                Err(Error::NotFound) => {
                    // Write to a virtual prefix dir is denied by construction.
                    if self.views.virtual_resolve(client_id, path).is_some() {
                        reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                    } else {
                        reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                    }
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                Err(err) => {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
        } else {
            match self.view_check_path_with_target(client_id, path) {
                Ok(pt) => pt,
                Err(Error::NotFound) => {
                    // Synthesise a virtual directory if path is a proper prefix
                    // of at least one mount destination in this client's view.
                    if let Some(entries) = self.views.virtual_resolve(client_id, path) {
                        let vfd = self.alloc_virtual_fd(entries);
                        reply_msg.words[0] = 0;
                        reply_msg.words[1] = vfd as usize;
                        reply_msg.words[2] = 0; // size=0 for dirs
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                    reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                Err(err) => {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
        };

        // #region agent log
        let _ = debug_print(&format!("vfs: open '{}' client={}", path, client_id));
        // #endregion

        if let view::MountTarget::MemFs { container_id } = target {
            return self.handle_open_memfs(
                container_id,
                &real_path,
                client_id,
                flags,
                reply_token,
                &mut reply_msg,
            );
        }

        // MountTable path: use unified mount table for all paths
        match self.mounts.open(&real_path, client_id) {
            Ok(file) => {
                if (flags & O_EXCL) != 0 && (flags & O_CREAT) != 0 {
                    reply_msg.words[0] = Error::AlreadyExists.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                // Track pts refcount before moving the file into the fd table.
                let pts_id = if let OpenFile::Pts(ref p) = file { Some(p.pts_id) } else { None };
                let size = file.size();
                let fd = self.files.open(client_id, file);
                if let Some(id) = pts_id {
                    self.pts_on_open(id);
                }
                reply_msg.words[0] = 0;
                reply_msg.words[1] = fd;
                reply_msg.words[2] = size;
            }
            Err(err) => {
                if err == Error::NotFound && (flags & O_CREAT) != 0 {
                    if let Err(policy_err) = self.ensure_create_allowed(client_id, path) {
                        reply_msg.words[0] = policy_err.to_errno() as usize;
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                    match self.mounts.create_file(&real_path, mode) {
                        Ok(()) => match self.mounts.open(&real_path, client_id) {
                            Ok(file) => {
                                self.set_owner(path, client_id);
                                let fd = self.files.open(client_id, file.clone());
                                reply_msg.words[0] = 0;
                                reply_msg.words[1] = fd;
                                reply_msg.words[2] = file.size();
                            }
                            Err(open_err) => {
                                reply_msg.words[0] = open_err.to_errno() as usize;
                            }
                        },
                        Err(create_err) => {
                            reply_msg.words[0] = create_err.to_errno() as usize;
                        }
                    }
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                // #region agent log
                if err != Error::NotFound {
                    let _ = debug_print(&format!("vfs: open FAILED {:?}", err));
                }
                // #endregion
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    /// MemFs dispatch for handle_open.
    fn handle_open_memfs(
        &mut self,
        container_id: u64,
        memfs_path: &str,
        client_id: usize,
        flags: usize,
        reply_token: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        // Scope MemFs operations so the immutable borrow of self.container_memfs
        // is released before we mutably borrow self.files.
        let memfs_result: Result<(usize, usize)> = {
            let Some(memfs_backend) = self.container_memfs.get(&container_id) else {
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                return ipc::reply(reply_token, reply_msg, IpcFlags::empty());
            };
            let open_result = memfs_backend.borrow().open(memfs_path);
            match open_result {
                Ok((inode_id, size)) => {
                    if (flags & O_EXCL) != 0 && (flags & O_CREAT) != 0 {
                        Err(Error::AlreadyExists)
                    } else if (flags & O_TRUNC) != 0 {
                        match memfs_backend.borrow_mut().truncate(inode_id, 0) {
                            Ok(()) => Ok((inode_id, 0)),
                            Err(err) => Err(err),
                        }
                    } else {
                        Ok((inode_id, size))
                    }
                }
                Err(Error::NotFound) if (flags & O_CREAT) != 0 => {
                    match memfs_backend.borrow_mut().create(memfs_path) {
                        Ok(inode_id) => Ok((inode_id, 0)),
                        Err(err) => Err(err),
                    }
                }
                Err(err) => Err(err),
            }
        };

        match memfs_result {
            Ok((inode_id, size)) => {
                let fd = self.files.open(
                    client_id,
                    OpenFile::MemFs(fd_table::MemFsEntry {
                        container_id,
                        inode_id,
                        memfs_path: alloc::string::String::from(memfs_path),
                        size,
                        rights: u64::MAX,
                    }),
                );
                reply_msg.words[0] = 0;
                reply_msg.words[1] = fd;
                reply_msg.words[2] = size;
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }

        ipc::reply(reply_token, reply_msg, IpcFlags::empty())
    }

    fn handle_close(
        &mut self,
        msg: &Message,
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_CLOSE, [0; 6], 1);
        let client_id = match self.resolve_client_id("close", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let fd = msg.words[2];
        // Virtual directory fd: remove from the synthetic table and reply OK.
        if fd >= 0x8000_0000 {
            self.virtual_fds.remove(&(fd as u32));
            reply_msg.words[0] = 0;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        // If this is a PTS fd, decrement the refcount before removing it.
        let pts_id = self
            .files
            .get(client_id, fd)
            .and_then(|f| if let OpenFile::Pts(ref p) = f { Some(p.pts_id) } else { None });
        self.files.close(client_id, fd);
        if let Some(id) = pts_id {
            self.pts_on_close(id);
        }
        reply_msg.words[0] = 0;
        if let Err(err) = ipc::reply(reply_token, &reply_msg, IpcFlags::empty()) {
            vfs_trace!("vfs: close reply failed {:?}", err);
        }
        Ok(())
    }

    fn handle_write(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let fd = msg.words[2];
        let offset = msg.words[3];
        let requested = msg.words[4];
        let mut reply_msg = Message::new(VFS_WRITE, [0; 6], 2);
        let client_id = match self.resolve_client_id("write", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let Some(entry) = self.files.get_mut(client_id, fd) else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        let to_write = requested.min(payload.len());
        let data = &payload[..to_write];

        match entry {
            OpenFile::Virtual(file) => {
                let end = offset.saturating_add(data.len());
                if end > file.data.len() {
                    file.data.resize(end, 0);
                }
                if offset < file.data.len() {
                    file.data[offset..end].copy_from_slice(data);
                    reply_msg.words[0] = 0;
                    reply_msg.words[1] = data.len();
                } else {
                    reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                }
            }
            OpenFile::Memory(_) => {
                reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
                reply_msg.words[1] = 0;
            }
            OpenFile::Device(device) => {
                use fd_table::DeviceType;
                match device.device_type {
                    DeviceType::Tty { .. }
                    | DeviceType::Tty0 { .. }
                    | DeviceType::Console { .. } => {
                        let (stored_ep, vt_idx) = match &device.device_type {
                            DeviceType::Tty { vt_index, endpoint } => (*endpoint, *vt_index as usize),
                            DeviceType::Tty0 { endpoint } => (*endpoint, 0usize),
                            DeviceType::Console { endpoint } => (*endpoint, 0usize),
                            _ => unreachable!(),
                        };
                        // Fallback to server-level tty_endpoints when stored_ep is 0
                        // (device was opened before the tty service registered).
                        let ep = if stored_ep != 0 {
                            stored_ep
                        } else {
                            self.tty_endpoints.get(vt_idx).copied().unwrap_or(0)
                        };
                        if ep != 0 {
                            // Forward write to tty endpoint.
                            let _ = ipc::send_with_payload(ep, libcluu::ipc::TTY_WRITE_LABEL, data);
                        }
                        reply_msg.words[0] = 0;
                        reply_msg.words[1] = data.len();
                    }
                    _ => {
                        // null/zero/urandom: accept and discard writes
                        reply_msg.words[0] = 0;
                        reply_msg.words[1] = data.len();
                    }
                }
            }
            OpenFile::Ext2(ext2) => {
                let endpoint = ext2.endpoint;
                let inode = ext2.inode;
                let old_size = ext2.size;
                match Self::write_remote_ext2(endpoint, inode, offset, data) {
                    Ok(written) => {
                        reply_msg.words[0] = 0;
                        reply_msg.words[1] = written;
                        if written > 0 {
                            let end = offset.saturating_add(written);
                            if end > ext2.size {
                                ext2.size = end;
                            }
                            self.cache.remove(inode, old_size);
                        }
                    }
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                        reply_msg.words[1] = 0;
                    }
                }
            }
            OpenFile::MemFs(memfs_entry) => {
                let cid = memfs_entry.container_id;
                let ino = memfs_entry.inode_id;
                if let Some(memfs_backend) = self.container_memfs.get(&cid) {
                    // Scope the borrow_mut so it's dropped before we borrow() for file_size.
                    let write_result = memfs_backend.borrow_mut().write(ino, offset, data);
                    match write_result {
                        Ok(written) => {
                            reply_msg.words[0] = 0;
                            reply_msg.words[1] = written;
                            // Update cached size in the fd entry.
                            memfs_entry.size = memfs_backend.borrow().file_size(ino);
                        }
                        Err(err) => {
                            reply_msg.words[0] = err.to_errno() as usize;
                            reply_msg.words[1] = 0;
                        }
                    }
                } else {
                    reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                    reply_msg.words[1] = 0;
                }
            }
            // PTS write: forward to the owning cluuterm via PTS_WRITE_LABEL.
            //
            // Fire-and-forget: VFS uses `send_with_payload`, not
            // `call_with_payload`. PTS write is naturally async — the
            // writer (e.g. /bin/login) does not need cluuterm's render
            // ack to make progress, and waiting for it creates a cross-
            // process sync-cross-call deadlock when cluuterm is itself
            // blocked on a VFS call (e.g., during its startup
            // posix_spawn/_close path).
            //
            // Tradeoff: we report all bytes as written even if cluuterm
            // never drains. Acceptable for PTS (cluuterm's recv queue
            // is per-window and not shared). Backpressure on cluuterm
            // queue overflow would manifest as kernel-level block of the
            // VFS thread inside `send_with_payload`; this is a known
            // limit, no worse than the prior blocking-call form.
            OpenFile::Pts(pts) => {
                let id = pts.pts_id;
                let ep = match self.pts_registry.notify_endpoint(id) {
                    Some(ep) if ep != 0 => ep,
                    _ => {
                        reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                        reply_msg.words[1] = 0;
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                };
                let req = Message::new(
                    libcluu::proto::pts::PTS_WRITE_LABEL,
                    [id as usize, data.len(), 0, 0, 0, 0],
                    2,
                );
                match ipc::send_msg_with_payload(ep, &req, data) {
                    Ok(_) => {
                        reply_msg.words[0] = 0;
                        reply_msg.words[1] = data.len();
                    }
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                        reply_msg.words[1] = 0;
                    }
                }
            }
        }

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn write_remote_ext2(endpoint: usize, inode: u32, offset: usize, data: &[u8]) -> Result<usize> {
        let req = Message::new(FS_WRITE, [0, 0, inode as usize, offset, data.len(), 0], 5);
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(endpoint, &req, data, &mut reply)?;
        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::from_errno(status));
        }
        Ok(reply.words[1])
    }

    fn handle_stat(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_STAT, [0; 6], 1);
        let client_id = match self.resolve_client_id("stat", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let path = match core::str::from_utf8(payload) {
            Ok(path) => path,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        // View check: resolve virtual path + target.
        let (real_path, target) = match self.view_check_path_with_target(client_id, path) {
            Ok(pt) => pt,
            Err(Error::NotFound) => {
                // Virtual prefix dir: synthesise a directory stat.
                if let Some(entries) = self.views.virtual_resolve(client_id, path) {
                    let info = StatInfo {
                        size: 0,
                        mode: (S_IFDIR | 0o555) as u32,
                        mtime: 0,
                        nlink: 2,
                        uid: 0,
                        gid: 0,
                        blocks: 0,
                    };
                    let _ = entries; // only needed for the Some guard
                    reply_msg.words[0] = 0;
                    let stat_bytes = info.to_bytes();
                    return reply_with_payload(reply_token, &reply_msg, &stat_bytes);
                }
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let stat_result = if let view::MountTarget::MemFs { container_id } = target {
            self.stat_memfs_path(container_id, &real_path)
        } else {
            self.stat_path(&real_path, client_id)
        };

        match stat_result {
            Ok(info) => {
                reply_msg.words[0] = 0;
                let stat_bytes = info.to_bytes();
                reply_with_payload(reply_token, &reply_msg, &stat_bytes)
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
            }
        }
    }

    fn handle_realpath(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_REALPATH, [0; 6], 2);
        let client_id = match self.resolve_client_id("realpath", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let path = match core::str::from_utf8(payload) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let real_path = match self.view_check_path(client_id, path) {
            Ok(rp) => rp,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let canon = match self.mounts.get_backend(&real_path) {
            Some(backend) => {
                let (prefix, rel) = self.mounts.split_path(&real_path);
                match backend.realpath(rel) {
                    Ok(rel_canon) => {
                        if rel_canon.starts_with('/') {
                            // Backend returned a path absolute within its
                            // own mount; re-prefix into a globally absolute
                            // path.
                            let mut out = alloc::string::String::from(prefix);
                            if out.ends_with('/') {
                                out.pop();
                            }
                            out.push_str(&rel_canon);
                            out
                        } else {
                            real_path.clone()
                        }
                    }
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                }
            }
            None => real_path.clone(),
        };
        let bytes = canon.into_bytes();
        reply_msg.words[1] = bytes.len();
        reply_with_payload(reply_token, &reply_msg, &bytes)
    }

    fn handle_fstat(
        &mut self,
        msg: &Message,
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        // fstat uses payload-based reply (v2 format).
        // words[1] = client_id, words[2] = fd (inline in message).
        let fd = msg.words[2];
        let mut reply_msg = Message::new(VFS_FSTAT, [0; 6], 1);
        let client_id = match self.resolve_client_id("fstat", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let Some(entry) = self.files.get(client_id, fd) else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        let sz = entry.size() as u64;
        let mode = match entry {
            OpenFile::Device(_) => S_IFCHR as u32 | 0o666,
            _ => MODE_FILE as u32,
        };
        let info = StatInfo {
            size: sz,
            mode,
            mtime: 0,
            nlink: 1,
            uid: 0,
            gid: 0,
            blocks: (sz + 511) / 512,
        };
        reply_msg.words[0] = 0;
        let stat_bytes = info.to_bytes();
        reply_with_payload(reply_token, &reply_msg, &stat_bytes)
    }

    fn handle_unlink(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        self.handle_single_path_mutation(
            msg,
            payload,
            reply_token,
            caller_client,
            SinglePathOp::Unlink,
        )
    }

    fn handle_mkdir(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mode = msg.words[2];
        self.handle_single_path_mutation(
            msg,
            payload,
            reply_token,
            caller_client,
            SinglePathOp::Mkdir { mode },
        )
    }

    fn handle_rmdir(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        self.handle_single_path_mutation(
            msg,
            payload,
            reply_token,
            caller_client,
            SinglePathOp::Rmdir,
        )
    }

    /// Shared body for unlink/mkdir/rmdir: parse path, view-check, dispatch to
    /// MemFs or backing mount, run permission gate, perform the op, update
    /// owner bookkeeping, invalidate cache, reply.
    fn handle_single_path_mutation(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
        op: SinglePathOp,
    ) -> Result<()> {
        let mut reply_msg = Message::new(op.reply_label(), [0; 6], 1);
        let client_id = match self.resolve_client_id(op.name(), caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let path = match core::str::from_utf8(payload) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let (real_path, target) = match self.view_check_path_writable_with_target(client_id, path) {
            Ok(pt) => pt,
            Err(Error::NotFound) => {
                // Deny writes to synthesised virtual prefix directories.
                let err = if self.views.virtual_resolve(client_id, path).is_some() {
                    Error::PermissionDenied
                } else {
                    Error::NotFound
                };
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        if let view::MountTarget::MemFs { container_id } = target {
            let memfs_result = self.get_container_memfs(container_id).and_then(|b| {
                let mut backend = b.borrow_mut();
                match op {
                    SinglePathOp::Unlink => backend.unlink(&real_path),
                    SinglePathOp::Mkdir { .. } => backend.mkdir(&real_path),
                    SinglePathOp::Rmdir => backend.rmdir(&real_path),
                }
            });
            match memfs_result {
                Ok(()) => reply_msg.words[0] = 0,
                Err(err) => reply_msg.words[0] = err.to_errno() as usize,
            }
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        let perm = match op {
            SinglePathOp::Unlink | SinglePathOp::Rmdir => {
                self.ensure_mutation_allowed(client_id, path)
            }
            SinglePathOp::Mkdir { .. } => self.ensure_create_allowed(client_id, path),
        };
        if let Err(err) = perm {
            reply_msg.words[0] = err.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        let mount_result = match op {
            SinglePathOp::Unlink => self.mounts.unlink(&real_path),
            SinglePathOp::Mkdir { mode } => self.mounts.mkdir(&real_path, mode),
            SinglePathOp::Rmdir => self.mounts.rmdir(&real_path),
        };
        match mount_result {
            Ok(()) => {
                reply_msg.words[0] = 0;
                match op {
                    SinglePathOp::Unlink => self.clear_owner_path(path),
                    SinglePathOp::Mkdir { .. } => self.set_owner(path, client_id),
                    SinglePathOp::Rmdir => self.clear_owner_subtree(path),
                }
                self.invalidate_cache_after_mutation();
            }
            Err(err) => reply_msg.words[0] = err.to_errno() as usize,
        }
        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn handle_rename(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_RENAME, [0; 6], 1);
        let client_id = match self.resolve_client_id("rename", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let old_len = msg.words[2];
        if old_len > payload.len() {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        let old_path = match core::str::from_utf8(&payload[..old_len]) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let new_path = match core::str::from_utf8(&payload[old_len..]) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        // View check: both paths must be in the view and writable.
        let (real_old, target_old) =
            match self.view_check_path_writable_with_target(client_id, old_path) {
                Ok(pt) => pt,
                Err(Error::NotFound) => {
                    let err = if self.views.virtual_resolve(client_id, old_path).is_some() {
                        Error::PermissionDenied
                    } else {
                        Error::NotFound
                    };
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                Err(err) => {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            };
        let (real_new, target_new) =
            match self.view_check_path_writable_with_target(client_id, new_path) {
                Ok(pt) => pt,
                Err(Error::NotFound) => {
                    let err = if self.views.virtual_resolve(client_id, new_path).is_some() {
                        Error::PermissionDenied
                    } else {
                        Error::NotFound
                    };
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                Err(err) => {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            };
        // Both paths must target the same backend.
        if let view::MountTarget::MemFs { container_id } = target_old {
            if target_old != target_new {
                reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            match self.get_container_memfs(container_id).and_then(|b| {
                b.borrow_mut().rename(&real_old, &real_new)
            }) {
                Ok(()) => reply_msg.words[0] = 0,
                Err(err) => reply_msg.words[0] = err.to_errno() as usize,
            }
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        if let Err(err) = self.ensure_mutation_allowed(client_id, old_path) {
            reply_msg.words[0] = err.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        if let Err(err) = self.ensure_create_allowed(client_id, new_path) {
            reply_msg.words[0] = err.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        match self.mounts.rename(&real_old, &real_new) {
            Ok(()) => {
                reply_msg.words[0] = 0;
                self.move_owner_subtree(old_path, new_path);
                self.invalidate_cache_after_mutation();
            }
            Err(err) => reply_msg.words[0] = err.to_errno() as usize,
        }
        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn handle_link(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_LINK, [0; 6], 1);
        let client_id = match self.resolve_client_id("link", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let old_len = msg.words[2];
        if old_len > payload.len() {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        let old_path = match core::str::from_utf8(&payload[..old_len]) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let new_path = match core::str::from_utf8(&payload[old_len..]) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        // View check: old must be readable, new must be writable.
        let (real_old, target_old) =
            match self.view_check_path_with_target(client_id, old_path) {
                Ok(pt) => pt,
                Err(err) => {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            };
        let (real_new, target_new) =
            match self.view_check_path_writable_with_target(client_id, new_path) {
                Ok(pt) => pt,
                Err(err) => {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            };
        if let view::MountTarget::MemFs { container_id } = target_old {
            if target_old != target_new {
                reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            // MemFs::link returns NotImplemented.
            match self.get_container_memfs(container_id).and_then(|b| {
                b.borrow().link(&real_old, &real_new)
            }) {
                Ok(()) => reply_msg.words[0] = 0,
                Err(err) => reply_msg.words[0] = err.to_errno() as usize,
            }
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        if let Err(err) = self.ensure_create_allowed(client_id, new_path) {
            reply_msg.words[0] = err.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        match self.mounts.link(&real_old, &real_new) {
            Ok(()) => {
                reply_msg.words[0] = 0;
            }
            Err(err) => reply_msg.words[0] = err.to_errno() as usize,
        }
        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn stat_path(&self, path: &str, caller_tid: usize) -> Result<StatInfo> {
        match self.mounts.stat_by_path(path, caller_tid) {
            Ok(s) => Ok(StatInfo {
                size: s.size,
                mode: s.mode,
                mtime: s.mtime,
                nlink: s.nlink,
                uid: s.uid,
                gid: s.gid,
                blocks: s.blocks,
            }),
            Err(_) => {
                if let Ok(file) = self.mounts.open(path, caller_tid) {
                    let (mode, mtime, nlink, uid, gid) = match &file {
                        OpenFile::Device(_) => (S_IFCHR as u32 | 0o666, 0u64, 1u32, 0u32, 0u32),
                        _ => (MODE_FILE as u32, 0, 1, 0, 0),
                    };
                    let sz = file.size() as u64;
                    return Ok(StatInfo {
                        size: sz,
                        mode,
                        mtime,
                        nlink,
                        uid,
                        gid,
                        blocks: (sz + 511) / 512,
                    });
                }
                Err(Error::NotFound)
            }
        }
    }

    fn ensure_mutation_allowed(&self, client_id: usize, path: &str) -> Result<()> {
        self.ensure_protected_path_allowed(path)?;
        if let Some(owner) = self.owner_of(path) {
            if owner != client_id {
                return Err(Error::PermissionDenied);
            }
        }
        Ok(())
    }

    fn ensure_create_allowed(&self, client_id: usize, path: &str) -> Result<()> {
        self.ensure_protected_path_allowed(path)?;
        if self.owner_of(path).is_some() {
            return Err(Error::AlreadyExists);
        }
        if let Some(parent) = parent_path(path) {
            if let Some(owner) = self.owner_of(&parent) {
                if owner != client_id {
                    return Err(Error::PermissionDenied);
                }
            }
        }
        Ok(())
    }

    fn ensure_protected_path_allowed(&self, path: &str) -> Result<()> {
        let protected = ["/bin", "/proc", "/dev/initrd", "/sys"];
        for prefix in &protected {
            if path == *prefix || path.starts_with(&format!("{}/", prefix)) {
                return Err(Error::PermissionDenied);
            }
        }
        Ok(())
    }

    fn normalize_path(path: &str) -> alloc::string::String {
        let mut out = alloc::string::String::from(path);
        if out.is_empty() {
            return alloc::string::String::from("/");
        }
        if !out.starts_with('/') {
            out.insert(0, '/');
        }
        while out.len() > 1 && out.ends_with('/') {
            out.pop();
        }
        out
    }

    fn owner_of(&self, path: &str) -> Option<usize> {
        let key = Self::normalize_path(path);
        self.path_owners.get(&key).copied()
    }

    fn set_owner(&mut self, path: &str, client_id: usize) {
        let key = Self::normalize_path(path);
        self.path_owners.insert(key, client_id);
    }

    fn clear_owner_path(&mut self, path: &str) {
        let key = Self::normalize_path(path);
        self.path_owners.remove(&key);
    }

    fn clear_owner_subtree(&mut self, root: &str) {
        let root_key = Self::normalize_path(root);
        let prefix = if root_key == "/" {
            alloc::string::String::from("/")
        } else {
            format!("{}/", root_key)
        };
        let keys: Vec<_> = self
            .path_owners
            .keys()
            .filter(|k| **k == root_key || k.starts_with(&prefix))
            .cloned()
            .collect();
        for key in keys {
            self.path_owners.remove(&key);
        }
    }

    fn move_owner_subtree(&mut self, from: &str, to: &str) {
        let from_key = Self::normalize_path(from);
        let to_key = Self::normalize_path(to);
        let from_prefix = if from_key == "/" {
            alloc::string::String::from("/")
        } else {
            format!("{}/", from_key)
        };
        let mut updates: Vec<(alloc::string::String, alloc::string::String, usize)> = Vec::new();
        for (path, owner) in &self.path_owners {
            if *path == from_key {
                updates.push((path.clone(), to_key.clone(), *owner));
            } else if path.starts_with(&from_prefix) {
                let suffix = &path[from_key.len()..];
                updates.push((path.clone(), format!("{}{}", to_key, suffix), *owner));
            }
        }
        for (old, _, _) in &updates {
            self.path_owners.remove(old);
        }
        for (_, new, owner) in updates {
            self.path_owners.insert(new, owner);
        }
    }

    fn handle_ring_setup(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_RING_SETUP, [0; 6], 4);
        let client_id = match self.resolve_client_id("ring_setup", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let Some(target_base) = parse_single_usize(payload) else {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };
        let target_space = msg.words[2];
        if target_space == 0 || !target_base.is_multiple_of(PAGE_SIZE) {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let requested = msg.words[3];
        let requested = requested.clamp(RING_MIN_REQUESTED_BYTES, RING_SLOT_CAPACITY);
        if requested < RING_MIN_REQUESTED_BYTES {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        if let Some(existing) = self.read_rings.get(&client_id).copied() {
            if existing.target_base != target_base || existing.target_space != target_space {
                reply_msg.words[0] = Error::Busy.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            let backing = unsafe {
                core::slice::from_raw_parts_mut(existing.source_base as *mut u8, existing.bytes)
            };
            if SharedRing::initialize(backing).is_err() {
                reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
            reply_msg.words[0] = 0;
            reply_msg.words[1] = existing.bytes;
            reply_msg.words[2] = requested.min(existing.capacity);
            reply_msg.words[3] = existing.slot;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let Some(slot) = self.free_ring_slots.pop() else {
            reply_msg.words[0] = Error::Busy.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };
        let session = ReadRingSession {
            source_base: self.ring_pool_base + slot * RING_SLOT_BYTES,
            target_base,
            target_space,
            bytes: RING_SLOT_BYTES,
            capacity: RING_SLOT_CAPACITY,
            slot,
        };

        let backing = unsafe {
            core::slice::from_raw_parts_mut(session.source_base as *mut u8, session.bytes)
        };
        if SharedRing::initialize(backing).is_err() {
            self.free_ring_slots.push(slot);
            reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let pages = session.bytes.div_ceil(PAGE_SIZE);
        for page_idx in 0..pages {
            let src = session.source_base + page_idx * PAGE_SIZE;
            let dst = session.target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, session.target_space, src, dst, 0x02) {
                self.free_ring_slots.push(slot);
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }

        self.read_rings.insert(client_id, session);
        reply_msg.words[0] = 0;
        reply_msg.words[1] = session.bytes;
        reply_msg.words[2] = requested.min(session.capacity);
        reply_msg.words[3] = session.slot;
        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    /// Establish a per-client bounce buffer for big single-shot reply payloads.
    /// Wire protocol:
    ///   request words: [target_base_len_unused, client_id, target_space]
    ///   request payload: target_base (usize LE bytes)
    ///   reply words:    [status, bytes, slot]
    /// Server-side bookkeeping lives in `BulkPool`; this handler only
    /// adapts the IPC wire format and dispatches.
    fn handle_bounce_setup(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_BOUNCE_SETUP, [0; 6], 3);
        let client_id = match self.resolve_client_id("bounce_setup", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let Some(target_base) = parse_single_usize(payload) else {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };
        let target_space = msg.words[2];

        match self
            .bounce_pool
            .setup(client_id, target_base, target_space, self.space_token)
        {
            Ok(session) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = session.bytes;
                reply_msg.words[2] = session.slot;
                ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
            }
        }
    }

    fn handle_read_ring(
        &mut self,
        msg: &Message,
        reply_token: usize,
        caller_client: Option<usize>,
        runtime: &mut Runtime,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_READ_RING, [0; 6], 4);
        let client_id = match self.resolve_client_id("read_ring", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let fd = msg.words[2];
        let offset = msg.words[3];
        let requested = msg.words[4];
        if requested == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            reply_msg.words[3] = 1;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let Some(session) = self.read_rings.get(&client_id).copied() else {
            reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };
        let Some(file) = self.files.get(client_id, fd).cloned() else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        let backing = unsafe {
            core::slice::from_raw_parts_mut(session.source_base as *mut u8, session.bytes)
        };
        let mut ring = match SharedRing::attach(backing) {
            Ok(ring) => ring,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        let max_fill = cmp::min(requested, ring.available_write());
        if max_fill == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = ring.notify_seq() as usize;
            reply_msg.words[3] = 0;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        // Async tty read: spawn IpcCallFuture to avoid blocking the recv loop.
        // Non-tty device types (null/zero/urandom/fb) stay on the sync path below.
        if let OpenFile::Device(device) = &file {
            use fd_table::DeviceType;
            if matches!(
                device.device_type,
                DeviceType::Tty { .. } | DeviceType::Tty0 { .. } | DeviceType::Console { .. }
            ) {
                let (stored_ep, vt_idx) = match &device.device_type {
                    DeviceType::Tty { vt_index, endpoint } => (*endpoint, *vt_index as usize),
                    DeviceType::Tty0 { endpoint } => (*endpoint, 0),
                    DeviceType::Console { endpoint } => (*endpoint, 0),
                    _ => (0, 0),
                };
                let ep = if stored_ep != 0 {
                    stored_ep
                } else {
                    self.tty_endpoints.get(vt_idx).copied().unwrap_or(0)
                };
                if ep == 0 {
                    reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                let req = Message::new(
                    libcluu::ipc::TTY_READ_REQUEST_LABEL,
                    [max_fill, 0, 0, 0, 0, 0],
                    1,
                );
                runtime.spawn(async move {
                    let result = IpcCallFuture::new(ep, req).await;
                    let completion = match result {
                        Ok((_reply, payload)) => Ok(payload),
                        Err(e) => Err(e),
                    };
                    libcluu::async_runtime::push_completion(
                        VfsCompletion::TtyReadRing {
                            reply_token,
                            client_id,
                            max_fill,
                            result: completion,
                        },
                    );
                });
                return Ok(());
            }
        }

        let data = self.read_file_chunk(&file, offset, max_fill)?;
        let pushed = ring.push(&data);
        let notify_seq = if pushed > 0 {
            ring.bump_notify_seq()
        } else {
            ring.notify_seq()
        };
        let eof = data.len() < max_fill;

        reply_msg.words[0] = 0;
        reply_msg.words[1] = pushed;
        reply_msg.words[2] = notify_seq as usize;
        reply_msg.words[3] = if eof { 1 } else { 0 };
        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn read_file_chunk(
        &mut self,
        file: &OpenFile,
        offset: usize,
        requested: usize,
    ) -> Result<Vec<u8>> {
        if requested == 0 {
            return Ok(Vec::new());
        }
        match file {
            OpenFile::Memory(entry) => {
                let available = entry.size.saturating_sub(offset);
                let len = requested.min(available);
                if len == 0 {
                    return Ok(Vec::new());
                }
                let src = unsafe {
                    core::slice::from_raw_parts(
                        (entry.base + entry.offset + offset) as *const u8,
                        len,
                    )
                };
                Ok(src.to_vec())
            }
            OpenFile::Virtual(vfile) => {
                let available = vfile.data.len().saturating_sub(offset);
                let len = requested.min(available);
                if len == 0 {
                    return Ok(Vec::new());
                }
                Ok(vfile.data[offset..offset + len].to_vec())
            }
            OpenFile::Ext2(entry) => {
                let available = entry.size.saturating_sub(offset);
                let len = requested.min(available).min(REMOTE_READ_CAP);
                if len == 0 {
                    return Ok(Vec::new());
                }
                if let Some(cache_entry) = self.cache.get(entry.inode, entry.size) {
                    let cached = unsafe {
                        core::slice::from_raw_parts(cache_entry.base as *const u8, cache_entry.len)
                    };
                    let cached_len = cached.len().saturating_sub(offset).min(len);
                    return Ok(cached[offset..offset + cached_len].to_vec());
                }
                if offset == 0 && requested >= entry.size {
                    if let Some(cache_entry) = self.cache_ext2_file(entry) {
                        let cached = unsafe {
                            core::slice::from_raw_parts(
                                cache_entry.base as *const u8,
                                cache_entry.len,
                            )
                        };
                        let cached_len = cached.len().min(len);
                        return Ok(cached[..cached_len].to_vec());
                    }
                }

                let mut out = alloc::vec![0u8; len];
                let read =
                    self.read_remote_chunk_into(entry.endpoint, entry.inode, offset, &mut out)?;
                out.truncate(read);
                Ok(out)
            }
            OpenFile::Device(device) => {
                use fd_table::DeviceType;
                match device.device_type {
                    DeviceType::Null => Ok(Vec::new()),
                    DeviceType::Zero => {
                        let len = requested.min(REMOTE_READ_CAP);
                        Ok(alloc::vec![0u8; len])
                    }
                    DeviceType::Urandom => {
                        let len = requested.min(REMOTE_READ_CAP);
                        let mut buf = alloc::vec![0u8; len];
                        unsafe { fill_random(buf.as_mut_ptr(), len) };
                        Ok(buf)
                    }
                    DeviceType::Tty { .. }
                    | DeviceType::Tty0 { .. }
                    | DeviceType::Console { .. } => {
                        // TTY reads are handled asynchronously by handle_read_ring
                        // via IpcCallFuture. Reaching here is a programming error.
                        Err(Error::InvalidState)
                    }
                    DeviceType::Fb { phys, size, width, height, pitch, bpp } => {
                        const FB_HEADER_MAGIC: u32 = 0x4642_4630;
                        let mut payload = [0u8; 40];
                        payload[0..4].copy_from_slice(&FB_HEADER_MAGIC.to_le_bytes());
                        payload[4..8].copy_from_slice(&width.to_le_bytes());
                        payload[8..12].copy_from_slice(&height.to_le_bytes());
                        payload[12..16].copy_from_slice(&pitch.to_le_bytes());
                        payload[16..20].copy_from_slice(&bpp.to_le_bytes());
                        payload[24..32].copy_from_slice(&size.to_le_bytes());
                        payload[32..40].copy_from_slice(&phys.to_le_bytes());
                        let off = offset;
                        if off >= payload.len() {
                            return Ok(Vec::new());
                        }
                        let n = (payload.len() - off).min(requested);
                        Ok(payload[off..off + n].to_vec())
                    }
                    DeviceType::Dynamic { .. } => {
                        Err(Error::InvalidState)
                    }
                }
            }
            OpenFile::MemFs(entry) => {
                match self.container_memfs.get(&entry.container_id) {
                    Some(backend) => backend.borrow().read(entry.inode_id, offset, requested),
                    None => Err(Error::NotFound),
                }
            }
            // PTS read: forward to the owning cluuterm via PTS_READ_LABEL.
            OpenFile::Pts(pts) => {
                let ep = match self.pts_registry.notify_endpoint(pts.pts_id) {
                    Some(ep) if ep != 0 => ep,
                    _ => return Err(Error::NotFound),
                };
                let req = Message::new(
                    libcluu::proto::pts::PTS_READ_LABEL,
                    [pts.pts_id as usize, requested, 0, 0, 0, 0],
                    2,
                );
                // PTS_WRITE_MAX +header is well under our IPC buffer cap.
                const PTS_REPLY_BUF: usize = 1024;
                let mut reply_buf = alloc::vec![0u8; PTS_REPLY_BUF];
                let (_reply, payload_len) =
                    ipc::call_with_reply_buf(ep, &req, &[], &mut reply_buf)?;
                let data_start = core::mem::size_of::<Message>();
                let data_len = payload_len.min(requested);
                if data_start + data_len > reply_buf.len() {
                    return Err(Error::InvalidState);
                }
                Ok(reply_buf[data_start..data_start + data_len].to_vec())
            }
        }
    }

    fn read_remote_chunk_into(
        &self,
        endpoint: usize,
        inode: u32,
        offset: usize,
        out: &mut [u8],
    ) -> Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let req = Message::new(
            FS_READ_GRANT,
            [0, 0, inode as usize, offset, out.len(), 0],
            5,
        );
        let mut reply = Message::new(0, [0; 6], 0);
        let mut payload = [0u8; TWO_USIZE_BYTES];
        payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
        payload[USIZE_BYTES..TWO_USIZE_BYTES]
            .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());
        ipc::call_with_payload(endpoint, &req, &payload, &mut reply)?;

        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::from_errno(status));
        }

        let bytes_read = reply.words[1].min(out.len());
        let page_offset = reply.words[2];
        if page_offset >= self.grant_buf_size {
            return Err(Error::InvalidState);
        }
        let available = self.grant_buf_size - page_offset;
        if bytes_read > available {
            return Err(Error::InvalidState);
        }
        let src = unsafe {
            core::slice::from_raw_parts(
                (self.grant_buf_base + page_offset) as *const u8,
                bytes_read,
            )
        };
        out[..bytes_read].copy_from_slice(src);
        Ok(bytes_read)
    }

    fn handle_read_grant(
        &mut self,
        msg: &Message,
        payload: &[u8],
        _reply_token: usize,
        caller_client: Option<usize>,
        runtime: &mut Runtime,
    ) -> Result<()> {
        let fd = msg.words[2];
        let offset = msg.words[3];
        let is_async = msg.tag.extra == libcluu::ipc::ASYNC_REPLY_TAG;
        let requested = if is_async {
            if payload.len() < core::mem::size_of::<usize>() * 3 {
                let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                let ep = self.endpoint;
                return ipc::reply_to_sender(msg, &reply_msg, ep, IpcFlags::empty());
            }
            let mut bytes = [0u8; core::mem::size_of::<usize>()];
            bytes.copy_from_slice(&payload[core::mem::size_of::<usize>() * 2..][..core::mem::size_of::<usize>()]);
            usize::from_ne_bytes(bytes)
        } else {
            msg.words[4]
        };
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        let ep = self.endpoint;
        let reply_simple = |rm: &Message| ipc::reply_to_sender(msg, rm, ep, IpcFlags::empty());
        let client_id = match self.resolve_client_id("read_grant", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return reply_simple(&reply_msg);
            }
        };
        vfs_trace!(
            "vfs: read_grant start client={} fd={} off={} req={}",
            client_id,
            fd,
            offset,
            requested
        );

        let Some((target_base, target_space)) = parse_usize_pair(payload) else {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return reply_simple(&reply_msg);
        };

        let Some(file) = self.files.get(client_id, fd).cloned() else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return reply_simple(&reply_msg);
        };

        if requested == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return reply_simple(&reply_msg);
        }

        if target_base & (PAGE_SIZE - 1) != 0 {
            reply_msg.words[0] = Error::InvalidArgument as isize as usize;
            return reply_simple(&reply_msg);
        }

        match file {
            OpenFile::Memory(entry) => {
                self.read_grant_memory(
                    &entry,
                    offset,
                    requested,
                    target_base,
                    target_space,
                    &mut reply_msg,
                )?;
            }
            OpenFile::Ext2(entry) => {
                if let Some(cache_entry) = self.cache.get(entry.inode, entry.size) {
                    self.read_grant_cached_region(
                        cache_entry.base,
                        cache_entry.len,
                        offset,
                        requested,
                        target_base,
                        target_space,
                        &mut reply_msg,
                    )?;
                } else if offset == 0 && requested >= entry.size {
                    if let Some(cache_entry) = self.cache_ext2_file(&entry) {
                        self.read_grant_cached_region(
                            cache_entry.base,
                            cache_entry.len,
                            offset,
                            requested,
                            target_base,
                            target_space,
                            &mut reply_msg,
                        )?;
                    } else {
                        self.read_grant_remote_chunked(
                            entry.endpoint,
                            entry.inode,
                            entry.size,
                            offset,
                            requested,
                            target_base,
                            target_space,
                            &mut reply_msg,
                        )?;
                    }
                } else {
                    // Direct chunked reads from disk when cache is unavailable.
                    self.read_grant_remote_chunked(
                        entry.endpoint,
                        entry.inode,
                        entry.size,
                        offset,
                        requested,
                        target_base,
                        target_space,
                        &mut reply_msg,
                    )?;
                }
            }
            OpenFile::Virtual(vfile) => {
                self.read_grant_virtual(
                    &vfile.data,
                    offset,
                    requested,
                    target_base,
                    target_space,
                    &mut reply_msg,
                )?;
            }
            OpenFile::Device(device) => {
                use fd_table::DeviceType;
                // Async tty read: spawn IpcCallFuture to avoid blocking the recv loop.
                // Non-tty device types (null/zero/urandom/fb) stay on the sync path below.
                if matches!(
                    device.device_type,
                    DeviceType::Tty { .. } | DeviceType::Tty0 { .. } | DeviceType::Console { .. }
                ) {
                    let (stored_ep, vt_idx) = match &device.device_type {
                        DeviceType::Tty { vt_index, endpoint } => (*endpoint, *vt_index as usize),
                        DeviceType::Tty0 { endpoint } => (*endpoint, 0),
                        DeviceType::Console { endpoint } => (*endpoint, 0),
                        _ => (0, 0),
                    };
                    let ep = if stored_ep != 0 {
                        stored_ep
                    } else {
                        self.tty_endpoints.get(vt_idx).copied().unwrap_or(0)
                    };
                    if ep == 0 {
                        reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
                        return reply_simple(&reply_msg);
                    }
                    let req = Message::new(
                        libcluu::ipc::TTY_READ_REQUEST_LABEL,
                        [requested, 0, 0, 0, 0, 0],
                        1,
                    );
                    runtime.spawn(async move {
                        let result = IpcCallFuture::new(ep, req).await;
                        let completion = match result {
                            Ok((_reply, payload)) => Ok(payload),
                            Err(e) => Err(e),
                        };
                        libcluu::async_runtime::push_completion(
                            VfsCompletion::TtyReadGrant {
                                reply_token: _reply_token,
                                target_base,
                                target_space,
                                result: completion,
                            },
                        );
                    });
                    return Ok(());
                }
                let _ = debug_print(&format!(
                    "vfs: read_grant device client={} fd={} device_type={:?}",
                    client_id, fd, core::mem::discriminant(&device.device_type)
                ));
                self.read_grant_device(
                    &device,
                    requested,
                    target_base,
                    target_space,
                    &mut reply_msg,
                )?;
                let _ = debug_print(&format!(
                    "vfs: read_grant device done client={} fd={} result={}",
                    client_id, fd, reply_msg.words[0]
                ));
            }
            OpenFile::MemFs(entry) => {
                let read_result: Result<Vec<u8>> = {
                    match self.container_memfs.get(&entry.container_id) {
                        Some(backend) => {
                            backend.borrow().read(entry.inode_id, offset, requested)
                        }
                        None => Err(Error::NotFound),
                    }
                };
                match read_result {
                    Ok(data) if !data.is_empty() => {
                        self.grant_data_to_caller(
                            &data,
                            target_base,
                            target_space,
                            &mut reply_msg,
                        )?;
                    }
                    Ok(_) => {
                        reply_msg.words[0] = 0;
                        reply_msg.words[1] = 0;
                        reply_msg.words[2] = 0;
                    }
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                    }
                }
            }
            // PTS read_grant (async path):
            //
            // Instead of synchronously calling cluuterm (which could deadlock
            // if cluuterm needs to call VFS while the reply is deferred), we:
            //   1. Park the shell's reply_token + caller context.
            //   2. Fire-and-forget PTS_READ_LABEL drain-hint to cluuterm.
            //   3. Return Ok(()) WITHOUT replying — shell stays blocked.
            //
            // Cluuterm drains cooked bytes and sends PTS_READ_DELIVER_LABEL
            // back; the handler below grants bytes into shell's space and
            // replies the parked token to unblock the shell.
            OpenFile::Pts(pts) => {
                let ep = match self.pts_registry.notify_endpoint(pts.pts_id) {
                    Some(ep) if ep != 0 => ep,
                    _ => {
                        reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                        return reply_simple(&reply_msg);
                    }
                };

                // Park the shell's blocked read.
                let (reply_ep, cookie) = if msg.tag.extra == libcluu::ipc::ASYNC_REPLY_TAG {
                    (
                        msg.words[libcluu::ipc::ASYNC_REPLY_EP_WORD],
                        msg.words[libcluu::ipc::ASYNC_REPLY_COOKIE_WORD],
                    )
                } else {
                    (0, 0)
                };
                let parked = ParkedRead {
                    reply_token: _reply_token,
                    reply_ep,
                    cookie,
                    caller_space: target_space,
                    target_base,
                    requested,
                };
                self.pending_pts_reads
                    .entry(pts.pts_id)
                    .or_insert_with(VecDeque::new)
                    .push_back(parked);

                // Drain-hint to cluuterm: words[0] clobbered by
                // send_msg_with_payload, so pts_id lives in words[1] and
                // requested lives in words[2].
                let drain_hint = Message::new(
                    libcluu::proto::pts::PTS_READ_LABEL,
                    [0, pts.pts_id as usize, requested, 0, 0, 0],
                    3,
                );
                let _ = ipc::send_msg_with_payload(ep, &drain_hint, &[]);
                // Shell stays blocked; we do NOT call ipc::reply here.
                return Ok(());
            }
        }

        reply_simple(&reply_msg)
    }

    fn read_grant_memory(
        &self,
        entry: &fd_table::FileEntry,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = entry.size.saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        vfs_trace!(
            "vfs: read_grant_memory len={} target_base={:#x} target_space={}",
            len,
            target_base,
            target_space
        );
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let file_base = entry.base + entry.offset + offset;
        let page_offset = file_base & (PAGE_SIZE - 1);
        let page_start = file_base - page_offset;
        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);

        // Show first 8 bytes of source data for debugging
        let src_preview =
            unsafe { core::slice::from_raw_parts(file_base as *const u8, 8.min(len)) };
        vfs_trace!(
            "vfs: granting {} pages from {:#x}, file_base={:#x}, first bytes={:02x?}",
            pages,
            page_start,
            file_base,
            src_preview
        );

        for page_idx in 0..pages {
            let src = page_start + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                vfs_trace!("vfs: space_grant failed: {:?}", err);
                reply_msg.words[0] = err.to_errno() as usize;
                return Ok(());
            }
        }
        vfs_trace!("vfs: grant successful");

        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        Ok(())
    }

    #[allow(dead_code)]
    fn read_grant_remote(
        &self,
        entry: &fd_table::Ext2Entry,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = entry.size.saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let req = Message::new(
            FS_READ_GRANT,
            [0, 0, entry.inode as usize, offset, len, 0],
            5,
        );
        let mut reply = Message::new(0, [0; 6], 0);
        let mut payload = [0u8; TWO_USIZE_BYTES];
        payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
        payload[USIZE_BYTES..TWO_USIZE_BYTES]
            .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());

        let result = ipc::call_with_payload(entry.endpoint, &req, &payload, &mut reply);
        match result {
            Ok(()) => {
                let status = reply.words[0] as isize;
                if status < 0 {
                    reply_msg.words[0] = status as usize;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = 0;
                    return Ok(());
                }

                let bytes_read = reply.words[1];
                let page_offset = reply.words[2];
                self.grant_buffer_to_caller(
                    bytes_read,
                    page_offset,
                    target_base,
                    target_space,
                    reply_msg,
                )
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
        }
    }

    /// Read from cached data (Vec in memory).
    fn read_grant_cached(
        &self,
        data: &[u8],
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = data.len().saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let slice = &data[offset..offset + len];
        self.grant_data_to_caller(slice, target_base, target_space, reply_msg)
    }

    #[allow(clippy::too_many_arguments)]
    fn read_grant_cached_region(
        &self,
        base: usize,
        len: usize,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let data = unsafe { core::slice::from_raw_parts(base as *const u8, len) };
        self.read_grant_cached(
            data,
            offset,
            requested,
            target_base,
            target_space,
            reply_msg,
        )
    }

    fn cache_ext2_file(&mut self, entry: &fd_table::Ext2Entry) -> Option<CacheEntry> {
        let base = self.cache.reserve(entry.size)?;
        if self
            .read_remote_into_cache(entry.endpoint, entry.inode, entry.size, base)
            .is_err()
        {
            self.cache.release_reserved(base, entry.size);
            return None;
        }
        self.cache.insert(entry.inode, entry.size, base, entry.size);
        // DIAG(unmap-bug): log every cache fill so we can see inode→VA mapping
        let _ = debug_print(&format!(
            "vfs: cache_fill inode={} size={} base={:#x}",
            entry.inode, entry.size, base
        ));
        self.cache.get(entry.inode, entry.size)
    }

    /// Walk `/var/images/*/manifest.toml`. For each container whose manifest
    /// contains `preload = true`, populate the ELF cache with every file in
    /// its `bin/` directory. Pays the disk read upfront so first-spawn
    /// latency drops to ~map_segments time only.
    ///
    /// Best-effort: any per-container failure is logged and skipped.
    fn preload_marked_binaries(&mut self) {
        let images_dir = "/var/images";
        let containers = match self.mounts.readdir(images_dir, 0) {
            Ok(v) => v,
            Err(err) => {
                let _ = debug_print(&format!(
                    "vfs: preload skipped — readdir {} failed: {:?}",
                    images_dir, err
                ));
                return;
            }
        };

        let mut preloaded = 0usize;
        for c_entry in containers {
            if !c_entry.is_dir || c_entry.name == "." || c_entry.name == ".." {
                continue;
            }
            let manifest_path = format!("{}/{}/manifest.toml", images_dir, c_entry.name);
            let manifest_bytes = match self.read_internal_file(&manifest_path) {
                Some(b) => b,
                None => continue,
            };
            let manifest_str = match core::str::from_utf8(&manifest_bytes) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if !manifest_has_preload(manifest_str) {
                continue;
            }

            let bin_dir = format!("{}/{}/bin", images_dir, c_entry.name);
            let bins = match self.mounts.readdir(&bin_dir, 0) {
                Ok(v) => v,
                Err(_) => continue,
            };
            for b_entry in bins {
                if b_entry.is_dir || b_entry.name == "." || b_entry.name == ".." {
                    continue;
                }
                let bin_path = format!("{}/{}", bin_dir, b_entry.name);
                let opened = match self.mounts.open(&bin_path, 0) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let entry = match opened {
                    OpenFile::Ext2(e) => e,
                    _ => continue,
                };
                if self.cache.get(entry.inode, entry.size).is_some() {
                    continue;
                }
                if entry.size > FILE_CACHE_MAX_SIZE {
                    continue;
                }
                if let Some(cache_entry) = self.cache_ext2_file(&entry) {
                    preloaded += 1;
                    let data = unsafe {
                        core::slice::from_raw_parts(cache_entry.base as *const u8, cache_entry.len)
                    };
                    let _ = self.cache.get_or_build_elf_meta(entry.inode, entry.size, data);
                }
            }
        }

        let _ = debug_print(&format!(
            "vfs: preloaded {} binaries from PRELOAD-marked containers",
            preloaded
        ));
    }

    /// Read a small file's complete contents through the local mount table
    /// (no view, no client tracking). Used by internal startup helpers like
    /// `preload_marked_binaries`.
    fn read_internal_file(&self, path: &str) -> Option<Vec<u8>> {
        let opened = self.mounts.open(path, 0).ok()?;
        let prefix = path;
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let chunk = self.mounts.read(prefix, &opened, offset, 4096).ok()?;
            if chunk.is_empty() {
                break;
            }
            offset += chunk.len();
            out.extend_from_slice(&chunk);
            if chunk.len() < 4096 {
                break;
            }
        }
        Some(out)
    }

    /// Read entire file from remote backend into the cache region.
    fn read_remote_into_cache(
        &self,
        endpoint: usize,
        inode: u32,
        size: usize,
        target_base: usize,
    ) -> Result<()> {
        let mut offset = 0;

        while offset < size {
            let remaining = size - offset;
            let chunk_size = remaining.min(REMOTE_READ_CAP);

            let req = Message::new(
                FS_READ_GRANT,
                [0, 0, inode as usize, offset, chunk_size, 0],
                5,
            );
            let mut reply = Message::new(0, [0; 6], 0);
            let mut payload = [0u8; TWO_USIZE_BYTES];
            payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
            payload[USIZE_BYTES..TWO_USIZE_BYTES]
                .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());

            ipc::call_with_payload(endpoint, &req, &payload, &mut reply)?;

            let status = reply.words[0] as isize;
            if status < 0 {
                return Err(Error::InvalidState);
            }

            let bytes_read = reply.words[1].min(chunk_size);
            let page_offset = reply.words[2];
            if page_offset >= self.grant_buf_size {
                return Err(Error::InvalidState);
            }
            let available = self.grant_buf_size - page_offset;
            if bytes_read > available {
                return Err(Error::InvalidState);
            }
            if bytes_read == 0 {
                return Err(Error::InvalidState);
            }

            let src = unsafe {
                core::slice::from_raw_parts(
                    (self.grant_buf_base + page_offset) as *const u8,
                    bytes_read,
                )
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    (target_base + offset) as *mut u8,
                    bytes_read,
                );
            }
            offset += bytes_read;
        }

        Ok(())
    }

    /// Chunked read from remote - used when file is too large to cache.
    #[allow(clippy::too_many_arguments)]
    fn read_grant_remote_chunked(
        &self,
        endpoint: usize,
        inode: u32,
        file_size: usize,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = file_size.saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        // #region agent log
        let _ = debug_print(&format!(
            "vfs: remote_chunked len={} offset={} target={:#x}",
            len, offset, target_base
        ));
        // #endregion
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let req = Message::new(FS_READ_GRANT, [0, 0, inode as usize, offset, len, 0], 5);
        let mut reply = Message::new(0, [0; 6], 0);
        let mut payload = [0u8; TWO_USIZE_BYTES];
        payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
        payload[USIZE_BYTES..TWO_USIZE_BYTES]
            .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());

        // #region agent log
        let _ = debug_print("vfs: calling blkdev read_grant");
        // #endregion
        let result = ipc::call_with_payload(endpoint, &req, &payload, &mut reply);
        match result {
            Ok(()) => {
                let status = reply.words[0] as isize;
                // #region agent log
                let _ = debug_print(&format!(
                    "vfs: blkdev replied status={} bytes={}",
                    status, reply.words[1]
                ));
                // #endregion
                if status < 0 {
                    reply_msg.words[0] = status as usize;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = 0;
                    return Ok(());
                }

                let bytes_read = reply.words[1];
                let page_offset = reply.words[2];
                self.grant_buffer_to_caller(
                    bytes_read,
                    page_offset,
                    target_base,
                    target_space,
                    reply_msg,
                )
            }
            Err(err) => {
                // #region agent log
                let _ = debug_print(&format!("vfs: blkdev call FAILED {:?}", err));
                // #endregion
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
        }
    }

    fn grant_buffer_to_caller(
        &self,
        len: usize,
        page_offset: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        // #region agent log
        let _ = debug_print(&format!(
            "vfs: grant_buffer len={} page_off={} target={:#x} space={}",
            len, page_offset, target_base, target_space
        ));
        // #endregion
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);
        if pages * PAGE_SIZE > self.grant_buf_size {
            // #region agent log
            let _ = debug_print("vfs: buffer too small!");
            // #endregion
            reply_msg.words[0] = Error::BufferTooSmall.to_errno() as usize;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        // #region agent log
        let _ = debug_print(&format!(
            "vfs: granting {} pages from {:#x} to {:#x}",
            pages, self.grant_buf_base, target_base
        ));
        // #endregion
        for page_idx in 0..pages {
            let src = self.grant_buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                // #region agent log
                let _ = debug_print(&format!(
                    "vfs: space_grant FAILED page {} src={:#x} dst={:#x} err={:?}",
                    page_idx, src, dst, err
                ));
                // #endregion
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                return Ok(());
            }
        }

        // #region agent log
        let _ = debug_print("vfs: grant_buffer OK");
        // #endregion
        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        Ok(())
    }

    fn read_grant_virtual(
        &self,
        data: &[u8],
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = data.len().saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let slice = &data[offset..offset + len];
        self.grant_data_to_caller(slice, target_base, target_space, reply_msg)
    }

    fn read_grant_device(
        &self,
        device: &fd_table::DeviceFile,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        use fd_table::DeviceType;

        match device.device_type {
            DeviceType::Null => {
                // /dev/null: always EOF
                reply_msg.words[0] = 0;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
            DeviceType::Zero => {
                // /dev/zero: fill grant buffer with zeroes, then grant in-place
                let len = requested.min(REMOTE_READ_CAP).min(self.grant_buf_size);
                if len == 0 {
                    reply_msg.words[0] = 0;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = 0;
                    return Ok(());
                }
                unsafe {
                    core::ptr::write_bytes(self.grant_buf_base as *mut u8, 0, len);
                }
                self.grant_buf_to_caller(len, target_base, target_space, reply_msg)
            }
            DeviceType::Urandom => {
                // /dev/urandom: fill grant buffer with random bytes, then grant in-place
                let len = requested.min(REMOTE_READ_CAP).min(self.grant_buf_size);
                if len == 0 {
                    reply_msg.words[0] = 0;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = 0;
                    return Ok(());
                }
                unsafe {
                    fill_random(self.grant_buf_base as *mut u8, len);
                }
                self.grant_buf_to_caller(len, target_base, target_space, reply_msg)
            }
            DeviceType::Tty { .. }
            | DeviceType::Tty0 { .. }
            | DeviceType::Console { .. } => {
                // TTY reads are handled asynchronously by handle_read_grant
                // via IpcCallFuture. Reaching here is a programming error.
                reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
            DeviceType::Fb { phys, size, width, height, pitch, bpp } => {
                // 40-byte little-endian FB geometry header:
                //   u32 magic, u32 width, u32 height, u32 pitch, u32 bpp,
                //   u32 reserved (zero), u64 size, u64 phys
                const FB_HEADER_MAGIC: u32 = 0x4642_4630;
                let mut payload = [0u8; 40];
                payload[0..4].copy_from_slice(&FB_HEADER_MAGIC.to_le_bytes());
                payload[4..8].copy_from_slice(&width.to_le_bytes());
                payload[8..12].copy_from_slice(&height.to_le_bytes());
                payload[12..16].copy_from_slice(&pitch.to_le_bytes());
                payload[16..20].copy_from_slice(&bpp.to_le_bytes());
                // bytes 20..24 reserved (zero)
                payload[24..32].copy_from_slice(&size.to_le_bytes());
                payload[32..40].copy_from_slice(&phys.to_le_bytes());
                let n = requested.min(payload.len());
                self.grant_data_to_caller(&payload[..n], target_base, target_space, reply_msg)
            }
            DeviceType::Dynamic { .. } => {
                reply_msg.words[0] = Error::InvalidState.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
        }
    }

    /// Grant pages from the pre-filled grant buffer to the caller's address space.
    /// Unlike `grant_data_to_caller`, this skips the copy step — data must already
    /// be written to `self.grant_buf_base`.
    fn grant_buf_to_caller(
        &self,
        len: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let pages = len.div_ceil(PAGE_SIZE);
        for page_idx in 0..pages {
            let src = self.grant_buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                return Ok(());
            }
        }
        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = 0;
        Ok(())
    }

    fn grant_data_to_caller(
        &self,
        data: &[u8],
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        vfs_trace!(
            "vfs: grant_data_to_caller start len={} target_base={:#x} target_space={}",
            data.len(),
            target_base,
            target_space
        );
        if data.is_empty() {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        if data.len() > self.grant_buf_size {
            vfs_trace!(
                "vfs: grant buffer too small len={} cap={}",
                data.len(),
                self.grant_buf_size
            );
            reply_msg.words[0] = Error::BufferTooSmall.to_errno() as usize;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        vfs_trace!(
            "vfs: grant_data len={} base={:#x} pages={}",
            data.len(),
            self.grant_buf_base,
            data.len().div_ceil(PAGE_SIZE)
        );
        // Copy data to the buffer
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.grant_buf_base as *mut u8,
                data.len(),
            );
        }

        // Grant the pages to the caller
        let pages = data.len().div_ceil(PAGE_SIZE);
        for page_idx in 0..pages {
            let src = self.grant_buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                reply_msg.words[0] = err.to_errno() as usize;
                return Ok(());
            }
        }

        reply_msg.words[0] = 0;
        reply_msg.words[1] = data.len();
        reply_msg.words[2] = 0;
        vfs_trace!("vfs: grant_data_to_caller done");
        Ok(())
    }

    /// Single-shot readdir with bounce-buffer fallback for big replies.
    /// Wire protocol:
    ///   request words: [path_len, client_id]
    ///   reply words:   [blob_len, status, count, bounce_flag]
    /// If `bounce_flag == 0`, blob follows inline as IPC payload.
    /// If `bounce_flag == 1`, client must read `blob_len` bytes from its
    /// bounce buffer at offset 0; IPC payload is empty.
    /// If client has no bounce set up and blob > inline limit, reply with
    /// status = BufferTooSmall (-10) so the client can set up a bounce
    /// buffer and retry.
    fn handle_readdir(
        &mut self,
        msg: &Message,
        payload: &[u8],
        _reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        const INLINE_BUDGET: usize = 3584;
        let ep = self.endpoint;
        let reply_simple = |rm: &Message| ipc::reply_to_sender(msg, rm, ep, IpcFlags::empty());
        let reply_blob = |rm: &Message, blob: &[u8]| ipc::reply_to_sender_with_payload(msg, rm, blob, ep);

        let mut reply_msg = Message::new(VFS_READDIR, [0; 6], 4);
        let client_id = match self.resolve_client_id("readdir", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[1] = err.to_errno() as usize;
                return reply_simple(&reply_msg);
            }
        };

        let path = match core::str::from_utf8(payload) {
            Ok(path) => path,
            Err(_) => {
                reply_msg.words[1] = Error::InvalidArgument.to_errno() as usize;
                return reply_simple(&reply_msg);
            }
        };

        let (real_path, target) = match self.view_check_path_with_target(client_id, path) {
            Ok(pt) => pt,
            Err(Error::NotFound) => {
                if let Some(entries) = self.views.virtual_resolve(client_id, path) {
                    let blob = build_virtual_readdir_blob(&entries);
                    let count = entries.len();
                    let blob_len = blob.len();
                    if blob_len <= INLINE_BUDGET {
                        reply_msg.words[0] = blob_len;
                        reply_msg.words[1] = 0;
                        reply_msg.words[2] = count;
                        reply_msg.words[3] = 0;
                        return reply_blob(&reply_msg, &blob);
                    }
                    let bounce = match self.bounce_pool.get(client_id) {
                        Some(b) => b,
                        None => {
                            reply_msg.words[1] = Error::BufferTooSmall.to_errno() as usize;
                            return reply_simple(&reply_msg);
                        }
                    };
                    if blob_len > bounce.bytes {
                        reply_msg.words[1] = Error::BufferTooSmall.to_errno() as usize;
                        return reply_simple(&reply_msg);
                    }
                    let dst = bounce.source_base as *mut u8;
                    unsafe { core::ptr::copy_nonoverlapping(blob.as_ptr(), dst, blob_len) };
                    reply_msg.words[0] = blob_len;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = count;
                    reply_msg.words[3] = 1;
                    return reply_simple(&reply_msg);
                }
                reply_msg.words[1] = Error::NotFound.to_errno() as usize;
                return reply_simple(&reply_msg);
            }
            Err(err) => {
                reply_msg.words[1] = err.to_errno() as usize;
                return reply_simple(&reply_msg);
            }
        };

        vfs_trace!("vfs: readdir '{}'", path);

        let (blob, count) = if let view::MountTarget::MemFs { container_id } = target {
            match self.build_readdir_blob_memfs(container_id, &real_path, client_id) {
                Ok(blob) => {
                    let count = count_readdir_entries(&blob);
                    (blob, count)
                }
                Err(err) => {
                    reply_msg.words[1] = err.to_errno() as usize;
                    return reply_simple(&reply_msg);
                }
            }
        } else {
            match self.mounts.readdir(&real_path, client_id) {
                Ok(mut entries) => {
                    let parent = path.trim_end_matches('/');
                    let parent = if parent.is_empty() { "/" } else { parent };
                    let push_child = |entries: &mut alloc::vec::Vec<crate::mount::DirEntry>, name: &str| {
                        if name.is_empty() || name.contains('/') {
                            return;
                        }
                        if entries.iter().any(|e| e.name == name) {
                            return;
                        }
                        entries.push(crate::mount::DirEntry {
                            name: alloc::string::String::from(name),
                            is_dir: true,
                            stat: crate::mount::DirEntryStat {
                                mode: 0o040555u32,
                                nlink: 1,
                                ..Default::default()
                            },
                        });
                    };
                    let mut wildcard_view = false;
                    if let Some(view) = self.views.get_view(client_id) {
                        for m in &view.mounts {
                            if m.dst == "/" {
                                wildcard_view = true;
                                continue;
                            }
                            let rest = if parent == "/" {
                                m.dst.strip_prefix('/')
                            } else {
                                m.dst.strip_prefix(parent)
                                    .and_then(|r| r.strip_prefix('/'))
                            };
                            if let Some(rest) = rest {
                                push_child(&mut entries, rest);
                            }
                        }
                    }
                    if wildcard_view {
                        for prefix in self.mounts.mount_prefixes() {
                            if prefix == "/" {
                                continue;
                            }
                            let rest = if parent == "/" {
                                prefix.strip_prefix('/')
                            } else {
                                prefix.strip_prefix(parent)
                                    .and_then(|r| r.strip_prefix('/'))
                            };
                            if let Some(rest) = rest {
                                push_child(&mut entries, rest);
                            }
                        }
                    }
                    let mut data = Vec::new();
                    let mut count = 0usize;
                    for entry in &entries {
                        let name_bytes = entry.name.as_bytes();
                        data.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
                        let stat_bytes = entry_to_stat_bytes(entry);
                        data.extend_from_slice(&stat_bytes);
                        data.extend_from_slice(name_bytes);
                        count += 1;
                    }
                    (data, count)
                }
                Err(err) => {
                    reply_msg.words[1] = err.to_errno() as usize;
                    return reply_simple(&reply_msg);
                }
            }
        };

        if blob.len() <= INLINE_BUDGET {
            reply_msg.words[0] = blob.len();
            reply_msg.words[1] = 0;
            reply_msg.words[2] = count;
            reply_msg.words[3] = 0;
            return reply_blob(&reply_msg, &blob);
        }

        let bounce = match self.bounce_pool.get(client_id) {
            Some(b) if blob.len() <= b.bytes => b,
            _ => {
                reply_msg.words[1] = Error::BufferTooSmall.to_errno() as usize;
                return reply_simple(&reply_msg);
            }
        };

        unsafe {
            core::ptr::copy_nonoverlapping(
                blob.as_ptr(),
                bounce.source_base as *mut u8,
                blob.len(),
            );
        }
        reply_msg.words[0] = blob.len();
        reply_msg.words[1] = 0;
        reply_msg.words[2] = count;
        reply_msg.words[3] = 1;
        reply_simple(&reply_msg)
    }

    /// Build the v2-format readdir blob for a MemFs-backed mount target.
    /// Used by the paginator on cursor=0; subsequent chunks read from the
    /// cached blob without rerunning this work.
    fn build_readdir_blob_memfs(
        &self,
        container_id: u64,
        memfs_path: &str,
        client_id: usize,
    ) -> Result<Vec<u8>> {
        let memfs_backend = self.get_container_memfs(container_id)?;
        let mut entries = memfs_backend.borrow().readdir(memfs_path)?;

        // For root readdir, merge in top-level dirs from the client's view
        // mounts so `ls /` shows /bin, /lib, /dev, etc.
        if memfs_path == "/" {
            if let Some(view) = self.views.get_view(client_id) {
                for m in &view.mounts {
                    let top = m.dst.strip_prefix('/').unwrap_or(&m.dst);
                    let top = match top.find('/') {
                        Some(pos) => &top[..pos],
                        None => top,
                    };
                    if top.is_empty() || top == "/" {
                        continue;
                    }
                    let already = entries.iter().any(|(name, _)| name == top);
                    if !already {
                        entries.push((alloc::string::String::from(top), true));
                    }
                }
            }
        }

        let fs_ref = memfs_backend.borrow();
        let mut data = Vec::new();
        for (name, is_dir) in &entries {
            let name_bytes = name.as_bytes();
            let entry_full = if memfs_path == "/" {
                alloc::format!("/{}", name)
            } else {
                alloc::format!("{}/{}", memfs_path.trim_end_matches('/'), name)
            };
            let (size, mode) = if *is_dir {
                (0u64, MODE_DIR as u32)
            } else {
                let sz = fs_ref.open(&entry_full).map(|(_, s)| s as u64).unwrap_or(0);
                (sz, MODE_FILE as u32)
            };
            let info = StatInfo {
                size,
                mode,
                mtime: 0,
                nlink: 1,
                uid: 0,
                gid: 0,
                blocks: (size + 511) / 512,
            };
            data.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
            data.extend_from_slice(&info.to_bytes());
            data.extend_from_slice(name_bytes);
        }
        Ok(data)
    }

    fn handle_map_elf(
        &mut self,
        msg: &Message,
        _reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let fd = msg.words[2];
        let target_space = msg.words[3];
        let map_start = self.clock_sample();
        let mut reply_msg = Message::new(VFS_MAP_ELF, [0; 6], 3);
        let _ = debug_print(&format!(
            "vfs: handle_map_elf START fd={} target_space={} cache_entries={}",
            fd, target_space, self.cache.entries.len()
        ));
        self.log_map_elf_stage(fd, "request", map_start);
        let client_id = match self.resolve_client_id("map_elf", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
            }
        };
        let Some(file) = self.files.get(client_id, fd).cloned() else {
            let _ = debug_print(&format!("vfs: map_elf miss client_id={} fd={}", client_id, fd));
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
        };

        match file {
            OpenFile::Ext2(entry) => {
                let cache_entry = if let Some(entry) = self.cache.get(entry.inode, entry.size) {
                    Some(entry)
                } else if entry.size <= FILE_CACHE_MAX_SIZE {
                    self.cache_ext2_file(&entry)
                } else {
                    None
                };

                let Some(cache_entry) = cache_entry else {
                    reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
                    return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
                };

                let _ = debug_print(&format!(
                    "vfs: map_elf inode={} cache_base={:#x} cache_len={} target_space={}",
                    entry.inode, cache_entry.base, cache_entry.len, target_space
                ));

                let data = unsafe {
                    core::slice::from_raw_parts(cache_entry.base as *const u8, cache_entry.len)
                };
                self.log_map_elf_stage(fd, "elf_cached", map_start);
                let elf_meta = match self
                    .cache
                    .get_or_build_elf_meta(entry.inode, entry.size, data)
                {
                    Ok(meta) => meta,
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                        return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
                    }
                };
                if let Err(err) = self.map_cached_elf_segments(target_space, &elf_meta, data) {
                    let _ = debug_print(&format!(
                        "vfs: map_elf FAILED inode={} err={:?}", entry.inode, err
                    ));
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
                }
                self.log_map_elf_stage(fd, "segments_mapped", map_start);
                let _ = debug_print(&format!(
                    "vfs: map_elf OK inode={} entry={:#x} target_space={}",
                    entry.inode, elf_meta.entry_point, target_space
                ));
                reply_msg.words[0] = 0;
                reply_msg.words[1] = elf_meta.entry_point;
                reply_msg.words[2] = cache_entry.len;
            }
            OpenFile::Memory(entry) => {
                let data = unsafe {
                    core::slice::from_raw_parts(
                        (entry.base + entry.offset) as *const u8,
                        entry.size,
                    )
                };
                let elf = match ElfFile::parse(data) {
                    Ok(elf) => elf,
                    Err(_) => {
                        reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                        return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
                    }
                };
                if let Err(err) = self.map_elf_segments(target_space, &elf, data) {
                    reply_msg.words[0] = err.to_errno() as usize;
                    return ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty());
                }
                self.log_map_elf_stage(fd, "segments_mapped", map_start);
                reply_msg.words[0] = 0;
                reply_msg.words[1] = elf.entry_point as usize;
                reply_msg.words[2] = entry.size;
            }
            OpenFile::Virtual(_) | OpenFile::Device(_) | OpenFile::MemFs(_) | OpenFile::Pts(_) => {
                reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
            }
        }

        self.log_map_elf_stage(fd, "reply", map_start);
        ipc::reply_to_sender(msg, &reply_msg, self.endpoint, IpcFlags::empty())
    }

    fn map_elf_segments(&self, target_space: usize, elf: &ElfFile, data: &[u8]) -> Result<()> {
        for segment in elf.segments_iter() {
            self.map_elf_segment(target_space, segment, data)?;
        }
        Ok(())
    }

    fn map_cached_elf_segments(
        &self,
        target_space: usize,
        elf_meta: &CachedElfMeta,
        data: &[u8],
    ) -> Result<()> {
        for segment in &elf_meta.segments {
            self.map_cached_elf_segment(target_space, *segment, data)?;
        }
        Ok(())
    }

    fn map_elf_segment(
        &self,
        target_space: usize,
        segment: &LoadableSegment,
        data: &[u8],
    ) -> Result<()> {
        let vaddr = segment.vaddr as usize;
        let mem_size = segment.mem_size as usize;
        if mem_size == 0 {
            return Ok(());
        }

        let file_offset = segment.file_offset as usize;
        let file_size = segment.file_size as usize;
        if file_offset + file_size > data.len() {
            return Err(Error::InvalidArgument);
        }

        // Handle non-page-aligned segments (e.g., .bss after .tdata/.tbss).
        // The first partial page was already mapped by the previous segment,
        // so skip it and only map from the next page boundary onward.
        let page_offset = vaddr & (PAGE_SIZE - 1);
        if page_offset != 0 {
            let next_page = (vaddr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            let end = vaddr + mem_size;
            if end <= next_page {
                // Entire segment fits within the already-mapped page.
                return Ok(());
            }
            let remaining = end - next_page;
            let num_pages = remaining.div_ceil(PAGE_SIZE);
            let skip = next_page - vaddr;
            let adj_file_size = file_size.saturating_sub(skip);
            let adj_file_offset = file_offset + file_size - adj_file_size;
            let data_ptr = data.as_ptr() as usize + adj_file_offset;
            return syscall::space_map_range(
                target_space,
                next_page,
                data_ptr,
                segment.page_flags() as usize,
                num_pages,
                adj_file_size,
            )
            .map(|_| ());
        }

        // Page-aligned case (common path).
        let num_pages = mem_size.div_ceil(PAGE_SIZE);
        let data_ptr = data.as_ptr() as usize + file_offset;

        syscall::space_map_range(
            target_space,
            vaddr,
            data_ptr,
            segment.page_flags() as usize,
            num_pages,
            file_size,
        )?;

        Ok(())
    }

    fn map_cached_elf_segment(
        &self,
        target_space: usize,
        segment: CachedElfSegment,
        data: &[u8],
    ) -> Result<()> {
        let vaddr = segment.vaddr;
        let mem_size = segment.mem_size;
        if mem_size == 0 {
            return Ok(());
        }

        let file_offset = segment.file_offset;
        let file_size = segment.file_size;
        if file_offset + file_size > data.len() {
            let _ = debug_print(&format!(
                "vfs: map_cached_seg BOUNDS_ERR vaddr={:#x} file_off={} file_sz={} data_len={}",
                vaddr, file_offset, file_size, data.len()
            ));
            return Err(Error::InvalidArgument);
        }

        // Handle non-page-aligned segments (e.g., .bss after .tdata/.tbss).
        // MAP_SHARE_PHYS re-enabled for the aliasing root-cause hunt. Audit
        // asserts in pmm::list_push/list_remove will trip on the first
        // double-alloc or double-free.
        let writable = (segment.page_flags & 0x02) != 0;
        let final_flags = if writable {
            segment.page_flags
        } else {
            segment.page_flags | libcluu::syscall::MAP_SHARE_PHYS
        };

        let page_offset = vaddr & (PAGE_SIZE - 1);
        if page_offset != 0 {
            let next_page = (vaddr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            let end = vaddr + mem_size;
            if end <= next_page {
                // DIAG(unmap-bug): segment entirely within already-mapped page — skipped
                let _ = debug_print(&format!(
                    "vfs: map_cached_seg SKIP_INPAGE vaddr={:#x} mem_sz={:#x} flags={:#x}",
                    vaddr, mem_size, final_flags
                ));
                return Ok(());
            }
            let remaining = end - next_page;
            let num_pages = remaining.div_ceil(PAGE_SIZE);
            let skip = next_page - vaddr;
            let adj_file_size = file_size.saturating_sub(skip);
            let adj_file_offset = file_offset + file_size - adj_file_size;
            let data_ptr = data.as_ptr() as usize + adj_file_offset;
            // DIAG(unmap-bug): non-aligned segment tail mapping
            let _ = debug_print(&format!(
                "vfs: map_cached_seg UNALIGNED vaddr={:#x} next_page={:#x} pages={} data_ptr={:#x} flags={:#x} adj_file_sz={}",
                vaddr, next_page, num_pages, data_ptr, final_flags, adj_file_size
            ));
            let r = syscall::space_map_range(
                target_space,
                next_page,
                data_ptr,
                final_flags,
                num_pages,
                adj_file_size,
            );
            if let Err(ref e) = r {
                let _ = debug_print(&format!(
                    "vfs: map_cached_seg UNALIGNED FAIL vaddr={:#x} pages={} err={:?}",
                    next_page, num_pages, e
                ));
            }
            return r.map(|_| ());
        }

        let num_pages = mem_size.div_ceil(PAGE_SIZE);
        let data_ptr = data.as_ptr() as usize + file_offset;
        // DIAG(unmap-bug): log every aligned segment mapping
        let _ = debug_print(&format!(
            "vfs: map_cached_seg vaddr={:#x} pages={} data_ptr={:#x} flags={:#x} file_sz={} share_phys={}",
            vaddr, num_pages, data_ptr, final_flags, file_size, !writable
        ));
        let r = syscall::space_map_range(
            target_space,
            vaddr,
            data_ptr,
            final_flags,
            num_pages,
            file_size,
        );
        if let Err(ref e) = r {
            let _ = debug_print(&format!(
                "vfs: map_cached_seg FAIL vaddr={:#x} pages={} err={:?}",
                vaddr, num_pages, e
            ));
        }
        r.map(|_| ())
    }
}

/// Convert a mount::DirEntry into a 40-byte v2 stat payload.
fn entry_to_stat_bytes(entry: &mount::DirEntry) -> [u8; 40] {
    let s = &entry.stat;
    let info = StatInfo {
        size: s.size,
        mode: s.mode,
        mtime: s.mtime,
        nlink: s.nlink,
        uid: s.uid,
        gid: s.gid,
        blocks: s.blocks,
    };
    info.to_bytes()
}

fn parent_path(path: &str) -> Option<alloc::string::String> {
    let mut norm = alloc::string::String::from(path);
    if norm.is_empty() {
        return None;
    }
    if !norm.starts_with('/') {
        norm.insert(0, '/');
    }
    while norm.len() > 1 && norm.ends_with('/') {
        norm.pop();
    }
    if norm == "/" {
        return None;
    }
    if let Some(pos) = norm.rfind('/') {
        if pos == 0 {
            return Some(alloc::string::String::from("/"));
        }
        return Some(alloc::string::String::from(&norm[..pos]));
    }
    None
}

/// Try to get a random u64 from the RDRAND instruction.
/// Returns `Some(value)` on success, `None` if RDRAND fails after retries.
fn rdrand64() -> Option<u64> {
    for _ in 0..10 {
        let value: u64;
        let ok: u8;
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {ok}",
                val = out(reg) value,
                ok = out(reg_byte) ok,
                options(nostack),
            );
        }
        if ok != 0 {
            return Some(value);
        }
    }
    None
}

/// Xorshift64 fallback PRNG for when RDRAND is unavailable.
fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Fill a buffer with random bytes, using RDRAND where available
/// and falling back to xorshift64.
///
/// # Safety
/// `buf` must point to at least `len` bytes of writable memory.
unsafe fn fill_random(buf: *mut u8, len: usize) {
    let mut offset = 0;
    // Try RDRAND for 8-byte chunks
    while offset + 8 <= len {
        let val = match rdrand64() {
            Some(v) => v,
            None => break,
        };
        core::ptr::write_unaligned(buf.add(offset) as *mut u64, val);
        offset += 8;
    }
    // If RDRAND worked for at least something, handle remaining bytes
    if offset > 0 && offset < len {
        if let Some(val) = rdrand64() {
            let remaining = len - offset;
            let bytes = val.to_ne_bytes();
            for (i, byte) in bytes.iter().enumerate().take(remaining) {
                *buf.add(offset + i) = *byte;
            }
            return;
        }
    }
    // Fallback: use xorshift64 for anything RDRAND didn't cover
    if offset < len {
        let seed = rdrand64().unwrap_or(0xDEAD_BEEF_CAFE_BABEu64);
        let mut state = seed;
        while offset + 8 <= len {
            let val = xorshift64(&mut state);
            core::ptr::write_unaligned(buf.add(offset) as *mut u64, val);
            offset += 8;
        }
        if offset < len {
            let val = xorshift64(&mut state);
            let bytes = val.to_ne_bytes();
            let remaining = len - offset;
            for (i, byte) in bytes.iter().enumerate().take(remaining) {
                *buf.add(offset + i) = *byte;
            }
        }
    }
}

fn parse_usize_pair(payload: &[u8]) -> Option<(usize, usize)> {
    if payload.len() < TWO_USIZE_BYTES {
        return None;
    }
    let mut bytes = [0u8; USIZE_BYTES];
    bytes.copy_from_slice(&payload[..USIZE_BYTES]);
    let first = usize::from_ne_bytes(bytes);
    bytes.copy_from_slice(&payload[USIZE_BYTES..TWO_USIZE_BYTES]);
    let second = usize::from_ne_bytes(bytes);
    Some((first, second))
}

fn parse_single_usize(payload: &[u8]) -> Option<usize> {
    if payload.len() < USIZE_BYTES {
        return None;
    }
    let mut bytes = [0u8; USIZE_BYTES];
    bytes.copy_from_slice(&payload[..USIZE_BYTES]);
    Some(usize::from_ne_bytes(bytes))
}

fn map_grant_buffer(space_token: usize) -> Result<usize> {
    let pages = GRANT_BUF_SIZE.div_ceil(PAGE_SIZE);
    if pages == 0 {
        return Err(Error::InvalidArgument);
    }

    match syscall::space_map_range(space_token, READ_BUF_BASE, 0, 0x03, pages, 0) {
        Ok(_) => {
            let _ = debug_print("vfs: grant buffer space_map_range ok");
            Ok(READ_BUF_BASE)
        }
        Err(Error::AlreadyExists) => {
            let _ = debug_print("vfs: grant buffer already mapped");
            Ok(READ_BUF_BASE)
        }
        Err(err) => {
            let _ = debug_print(&format!("vfs: grant buffer map failed {:?}", err));
            Err(err)
        }
    }
}

fn map_cache_buffer(space_token: usize) -> Result<usize> {
    let pages = CACHE_BUF_SIZE.div_ceil(PAGE_SIZE);
    if pages == 0 {
        return Err(Error::InvalidArgument);
    }

    // Kernel caps a single space_map_range at 32768 pages (128 MB). Chunk
    // larger requests so we can map up to whatever CACHE_BUF_SIZE the
    // build chose.
    const CHUNK_PAGES: usize = 32768;
    let mut mapped = 0usize;
    while mapped < pages {
        let remaining = pages - mapped;
        let this_chunk = remaining.min(CHUNK_PAGES);
        let virt = CACHE_BUF_BASE + mapped * PAGE_SIZE;
        match syscall::space_map_range(space_token, virt, 0, 0x03, this_chunk, 0) {
            Ok(_) => {}
            Err(Error::AlreadyExists) => {
                let _ = debug_print("vfs: cache buffer chunk already mapped");
            }
            Err(err) => {
                let _ = debug_print(&format!(
                    "vfs: cache buffer map failed at virt={:#x} chunk_pages={} err={:?}",
                    virt, this_chunk, err
                ));
                return Err(err);
            }
        }
        mapped += this_chunk;
    }
    let _ = debug_print("vfs: cache buffer space_map_range ok");
    Ok(CACHE_BUF_BASE)
}

fn map_ring_pool(space_token: usize) -> Result<usize> {
    let pages = RING_POOL_SIZE.div_ceil(PAGE_SIZE);
    if pages == 0 {
        return Err(Error::InvalidArgument);
    }

    match syscall::space_map_range(space_token, RING_POOL_BASE, 0, 0x03, pages, 0) {
        Ok(_) => {
            let _ = debug_print("vfs: ring pool space_map_range ok");
            Ok(RING_POOL_BASE)
        }
        Err(Error::AlreadyExists) => {
            let _ = debug_print("vfs: ring pool already mapped");
            Ok(RING_POOL_BASE)
        }
        Err(err) => {
            let _ = debug_print(&format!("vfs: ring pool map failed {:?}", err));
            Err(err)
        }
    }
}

fn map_bounce_pool(space_token: usize) -> Result<usize> {
    let pages = BOUNCE_POOL_SIZE.div_ceil(PAGE_SIZE);
    if pages == 0 {
        return Err(Error::InvalidArgument);
    }

    match syscall::space_map_range(space_token, BOUNCE_POOL_BASE, 0, 0x03, pages, 0) {
        Ok(_) => {
            let _ = debug_print("vfs: bounce pool space_map_range ok");
            Ok(BOUNCE_POOL_BASE)
        }
        Err(Error::AlreadyExists) => {
            let _ = debug_print("vfs: bounce pool already mapped");
            Ok(BOUNCE_POOL_BASE)
        }
        Err(err) => {
            let _ = debug_print(&format!("vfs: bounce pool map failed {:?}", err));
            Err(err)
        }
    }
}

/// Serialize a list of virtual directory names into the v2 readdir blob format.
/// All entries are emitted as directories (S_IFDIR | 0o555).
fn build_virtual_readdir_blob(names: &[alloc::string::String]) -> Vec<u8> {
    let mut data = Vec::new();
    let mode = (S_IFDIR | 0o555) as u32;
    for name in names {
        let name_bytes = name.as_bytes();
        let info = StatInfo {
            size: 0,
            mode,
            mtime: 0,
            nlink: 2,
            uid: 0,
            gid: 0,
            blocks: 0,
        };
        data.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        data.extend_from_slice(&info.to_bytes());
        data.extend_from_slice(name_bytes);
    }
    data
}

/// Count v2-format readdir entries in a serialized blob.
fn count_readdir_entries(blob: &[u8]) -> usize {
    let mut off = 0usize;
    let mut count = 0usize;
    while off + 44 <= blob.len() {
        let name_len = u32::from_le_bytes([
            blob[off], blob[off + 1], blob[off + 2], blob[off + 3],
        ]) as usize;
        let entry_size = 4 + 40 + name_len;
        if off + entry_size > blob.len() {
            break;
        }
        off += entry_size;
        count += 1;
    }
    count
}
