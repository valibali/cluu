//! virtio-gpu wire protocol structs and constants (VirtIO 1.3 §5.7).
//!
//! All fields are little-endian. On x86_64 native u32/u64 are already LE,
//! so #[repr(C)] struct reads/writes produce the correct wire format.
//!
//! Classic 2D only — no virgl, no blobs, no resource UUID. Cursorq is
//! set up but no cursor commands are implemented (deferred).

// ── Virtqueue indices (§5.7.1) ───────────────────────────────────────────

/// Control queue — host→guest commands and responses.
pub const VQ_CONTROL: usize = 0;
/// Cursor queue — cursor commands (deferred; queue is still set up).
pub const VQ_CURSOR: usize = 1;
pub const NUM_VQS: usize = 2;

// ── Feature bits (§5.7.4) ────────────────────────────────────────────────

/// Virgl 3D — REJECTED (classic 2D only).
pub const VIRTIO_GPU_F_VIRGL: u64 = 1 << 0;
/// EDID — optional, accepted if present.
pub const VIRTIO_GPU_F_EDID: u64 = 1 << 1;
/// Resource UUID — REJECTED.
pub const VIRTIO_GPU_F_RESOURCE_UUID: u64 = 1 << 2;
/// Resource blob — REJECTED.
pub const VIRTIO_GPU_F_RESOURCE_BLOB: u64 = 1 << 3;

/// Features the driver explicitly rejects (must not be negotiated).
pub const REJECTED_FEATURES: u64 =
    VIRTIO_GPU_F_VIRGL | VIRTIO_GPU_F_RESOURCE_UUID | VIRTIO_GPU_F_RESOURCE_BLOB;

// ── Control header flags (§5.7.5) ────────────────────────────────────────

/// Set in ctrl_hdr.flags when fence_id is valid.
pub const VIRTIO_GPU_FLAG_FENCE: u32 = 1;

// ── Device config events (§5.7.4.1) ──────────────────────────────────────

/// Display configuration changed — re-query GET_DISPLAY_INFO.
pub const VIRTIO_GPU_EVENT_DISPLAY: u32 = 1 << 0;

// ── Command types (§5.7.5) — host→device (controlq) ──────────────────────

pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const VIRTIO_GPU_CMD_GET_EDID: u32 = 0x0108;

// ── Response types (§5.7.5) — device→host ────────────────────────────────

pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_OK_EDID: u32 = 0x1102;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
pub const VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
pub const VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
pub const VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
pub const VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

// ── 2D resource formats (§5.7.6) ─────────────────────────────────────────

pub const VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM: u32 = 1;
pub const VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: u32 = 2;
pub const VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM: u32 = 3;
pub const VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM: u32 = 4;
pub const VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM: u32 = 67;
pub const VIRTIO_GPU_FORMAT_X8B8G8R8_UNORM: u32 = 68;
pub const VIRTIO_GPU_FORMAT_A8B8G8R8_UNORM: u32 = 69;

/// Maximum scanouts the device can report.
pub const VIRTIO_GPU_MAX_SCANOUTS: usize = 16;

// ── Wire structs ─────────────────────────────────────────────────────────

/// Common control header — prefix of every command AND every response (24B).
///
/// For commands: `type` is a `VIRTIO_GPU_CMD_*` value.
/// For responses: `type` is a `VIRTIO_GPU_RESP_*` value.
/// `flags` may carry VIRTIO_GPU_FLAG_FENCE; `fence_id` is valid when set.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct CtrlHdr {
    pub type_: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub padding: u32,
}

/// Rectangle used by SET_SCANOUT, TRANSFER_TO_HOST_2D, RESOURCE_FLUSH.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One scanout mode in GET_DISPLAY_INFO response.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct DisplayOne {
    pub r: Rect,
    pub enabled: u32,
    pub flags: u32,
}

/// GET_DISPLAY_INFO response (§5.7.5.1): header + up to 16 scanout modes.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct RespDisplayInfo {
    pub hdr: CtrlHdr,
    pub pmodes: [DisplayOne; VIRTIO_GPU_MAX_SCANOUTS],
}

/// RESOURCE_CREATE_2D command (§5.7.5.2).
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ResourceCreate2d {
    pub hdr: CtrlHdr,
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// RESOURCE_UNREF command.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ResourceUnref {
    pub hdr: CtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
}

/// SET_SCANOUT command (§5.7.5.4).
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct SetScanout {
    pub hdr: CtrlHdr,
    pub r: Rect,
    pub scanout_id: u32,
    pub resource_id: u32,
}

/// RESOURCE_FLUSH command (§5.7.5.5).
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ResourceFlush {
    pub hdr: CtrlHdr,
    pub r: Rect,
    pub resource_id: u32,
    pub padding: u32,
}

/// TRANSFER_TO_HOST_2D command (§5.7.5.6).
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct TransferToHost2d {
    pub hdr: CtrlHdr,
    pub r: Rect,
    pub offset: u64,
    pub resource_id: u32,
    pub padding: u32,
}

/// RESOURCE_ATTACH_BACKING command (§5.7.5.7). The SG entries follow as
/// separate OUT descriptors in the same chain.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ResourceAttachBacking {
    pub hdr: CtrlHdr,
    pub resource_id: u32,
    pub nr_entries: u32,
}

/// RESOURCE_DETACH_BACKING command.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ResourceDetachBacking {
    pub hdr: CtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
}

/// Scatter-gather memory entry — one per backing page/segment.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct MemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}

/// Device configuration space (§5.7.4), read via device_cfg capability.
///
/// `events_read` is set by the device when a display configuration change
/// occurs. The driver writes the same bits to `events_clear` to ack.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct GpuConfig {
    pub events_read: u32,
    pub events_clear: u32,
    pub num_scanouts: u32,
    pub num_capsets: u32,
}

// ── Compile-time layout assertions ───────────────────────────────────────
//
// These guard against struct padding drift that would corrupt the wire
// format. Uses const assertions (no external crate needed).

const fn assert_eq_val(a: usize, b: usize) {
    assert!(a == b);
}

const _: () = assert_eq_val(core::mem::size_of::<CtrlHdr>(), 24);
const _: () = assert_eq_val(core::mem::size_of::<Rect>(), 16);
const _: () = assert_eq_val(core::mem::size_of::<DisplayOne>(), 24);
const _: () = assert_eq_val(
    core::mem::size_of::<RespDisplayInfo>(),
    24 + 24 * VIRTIO_GPU_MAX_SCANOUTS,
);
const _: () = assert_eq_val(core::mem::size_of::<ResourceCreate2d>(), 40);
const _: () = assert_eq_val(core::mem::size_of::<ResourceUnref>(), 32);
const _: () = assert_eq_val(core::mem::size_of::<SetScanout>(), 48);
const _: () = assert_eq_val(core::mem::size_of::<ResourceFlush>(), 48);
const _: () = assert_eq_val(core::mem::size_of::<TransferToHost2d>(), 56);
const _: () = assert_eq_val(core::mem::size_of::<ResourceAttachBacking>(), 32);
const _: () = assert_eq_val(core::mem::size_of::<ResourceDetachBacking>(), 32);
const _: () = assert_eq_val(core::mem::size_of::<MemEntry>(), 16);
const _: () = assert_eq_val(core::mem::size_of::<GpuConfig>(), 16);

/// Check if a response type indicates success.
pub const fn resp_ok(resp_type: u32) -> bool {
    resp_type == VIRTIO_GPU_RESP_OK_NODATA
        || resp_type == VIRTIO_GPU_RESP_OK_DISPLAY_INFO
        || resp_type == VIRTIO_GPU_RESP_OK_EDID
}

/// Decode a response type into a human-readable string for diagnostics.
pub fn resp_name(resp_type: u32) -> &'static str {
    match resp_type {
        VIRTIO_GPU_RESP_OK_NODATA => "OK_NODATA",
        VIRTIO_GPU_RESP_OK_DISPLAY_INFO => "OK_DISPLAY_INFO",
        VIRTIO_GPU_RESP_OK_EDID => "OK_EDID",
        VIRTIO_GPU_RESP_ERR_UNSPEC => "ERR_UNSPEC",
        VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY => "ERR_OUT_OF_MEMORY",
        VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID => "ERR_INVALID_SCANOUT_ID",
        VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID => "ERR_INVALID_RESOURCE_ID",
        VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID => "ERR_INVALID_CONTEXT_ID",
        VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER => "ERR_INVALID_PARAMETER",
        _ => "UNKNOWN",
    }
}
