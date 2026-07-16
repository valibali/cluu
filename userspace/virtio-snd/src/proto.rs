//! virtio-snd wire protocol structs and constants (virtio 1.2 §5.14).
//!
//! All fields are little-endian. On x86_64 native u32/u64 are already LE,
//! so #[repr(C)] struct reads/writes produce the correct wire format.

// ── Virtqueue indices (§5.14.2) ──────────────────────────────────────────

pub const VQ_CONTROL: usize = 0;
pub const VQ_EVENT: usize = 1;
pub const VQ_TX: usize = 2;
pub const VQ_RX: usize = 3;
pub const NUM_VQS: usize = 4;

// ── Control opcodes (§5.14.6) ────────────────────────────────────────────

pub const R_JACK_INFO: u32 = 0x0001;
pub const R_PCM_INFO: u32 = 0x0100;
pub const R_PCM_SET_PARAMS: u32 = 0x0101;
pub const R_PCM_PREPARE: u32 = 0x0102;
pub const R_PCM_RELEASE: u32 = 0x0103;
pub const R_PCM_START: u32 = 0x0104;
pub const R_PCM_STOP: u32 = 0x0105;
pub const R_CHMAP_INFO: u32 = 0x0200;

// ── Event types ──────────────────────────────────────────────────────────

pub const EVT_JACK_CONNECTED: u32 = 0x1000;
pub const EVT_JACK_DISCONNECTED: u32 = 0x1001;
pub const EVT_PCM_PERIOD_ELAPSED: u32 = 0x1100;
pub const EVT_PCM_XRUN: u32 = 0x1101;

// ── Status codes ─────────────────────────────────────────────────────────

pub const S_OK: u32 = 0x8000;
pub const S_BAD_MSG: u32 = 0x8001;
pub const S_NOT_SUPP: u32 = 0x8002;
pub const S_IO_ERR: u32 = 0x8003;

// ── PCM formats (§5.14.6.8) ──────────────────────────────────────────────

pub const PCM_FMT_S16: u8 = 5; // signed 16-bit LE
pub const PCM_FMT_S32: u8 = 18;

// ── PCM rates ────────────────────────────────────────────────────────────

pub const PCM_RATE_5512: u8 = 0;
pub const PCM_RATE_8000: u8 = 1;
pub const PCM_RATE_11025: u8 = 2;
pub const PCM_RATE_16000: u8 = 3;
pub const PCM_RATE_22050: u8 = 4;
pub const PCM_RATE_32000: u8 = 5;
pub const PCM_RATE_44100: u8 = 6;
pub const PCM_RATE_48000: u8 = 7;
pub const PCM_RATE_64000: u8 = 8;
pub const PCM_RATE_88200: u8 = 9;
pub const PCM_RATE_96000: u8 = 10;

// ── Stream direction ─────────────────────────────────────────────────────

pub const D_OUTPUT: u8 = 0;
pub const D_INPUT: u8 = 1;

// ── Wire structs ─────────────────────────────────────────────────────────

/// Common header for all control queue messages (4 bytes).
#[repr(C)]
pub struct SndHdr {
    pub code: u32,
}

/// PCM-specific header: adds stream_id (8 bytes total).
#[repr(C)]
pub struct PcmHdr {
    pub hdr: SndHdr,
    pub stream_id: u32,
}

/// PCM set parameters request (24 bytes).
#[repr(C)]
pub struct PcmSetParams {
    pub hdr: PcmHdr,
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub features: u32,
    pub channels: u8,
    pub format: u8,
    pub rate: u8,
    pub padding: u8,
}

/// TX queue transfer header (4 bytes, OUT descriptor).
#[repr(C)]
pub struct PcmXfer {
    pub stream_id: u32,
}

/// TX queue completion status (8 bytes, IN descriptor).
#[repr(C)]
pub struct PcmStatus {
    pub status: u32,
    pub latency_bytes: u32,
}

/// Device configuration space (16 bytes, read via device_cfg capability).
#[repr(C)]
pub struct SndConfig {
    pub jacks: u32,
    pub streams: u32,
    pub chmaps: u32,
    pub controls: u32,
}

/// Event queue entry (8 bytes, IN descriptor filled by device).
#[repr(C)]
pub struct SndEvent {
    pub code: u32,
    pub data: u32,
}
