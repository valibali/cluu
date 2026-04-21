#![no_std]
#![no_main]

//! TPM 2.0 daemon — TIS MMIO driver + IPC service.
//!
//! Probes for a TPM 2.0 device at the standard TIS MMIO address, sends
//! TPM2_Startup, then enters an IPC server loop handling PCR read/extend
//! and info queries.  If no TPM is present, runs in stub mode (all
//! commands return ENODEV).

extern crate alloc;

use alloc::format;
use core::mem::size_of;
use libcluu::boot::{process_info, TOKEN_IPC, TOKEN_SPACE};
use libcluu::crypto::sha256;
use libcluu::device_io::{DeviceIo, MmioRegion};
use libcluu::ipc::{extract_reply_id, reply, reply_with_payload};
use libcluu::syscall::{endpoint_create, ipc_recv_any, space_map, MAP_DEVICE};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, yield_cpu, Result};

// ── MMIO layout ─────────────────────────────────────────────────────────

const TPM_MMIO_PHYS: usize = 0xFED4_0000;
const TPM_MMIO_VIRT: usize = 0x5000_0000;

// TIS register offsets
const REG_ACCESS: u16    = 0x000;
const REG_STS: u16       = 0x018;
const REG_DATA_FIFO: u16 = 0x024;
const REG_DID_VID: u16   = 0xF00;

// ACCESS register bits
const ACCESS_ACTIVE: u8  = 1 << 5;
const ACCESS_REQUEST: u8 = 1 << 1;

// STS register bits
const STS_VALID: u32     = 1 << 7;
const STS_CMD_READY: u32 = 1 << 6;
const STS_GO: u32        = 1 << 5;
const STS_DATA_AVAIL: u32 = 1 << 4;
const STS_EXPECT: u32    = 1 << 3;

const TIMEOUT: u32 = 100_000;

// ── IPC labels (must match libcluu::ipc) ────────────────────────────────

const LABEL_STARTUP: u32        = 1;
const LABEL_PCR_READ: u32       = 2;
const LABEL_PCR_EXTEND: u32     = 3;
const LABEL_GET_INFO: u32       = 4;
const LABEL_CREATE_PRIMARY: u32 = 5;
const LABEL_SEAL: u32           = 6;
const LABEL_UNSEAL: u32         = 7;
const LABEL_CREATE_AIK: u32     = 8;
const LABEL_QUOTE: u32          = 9;

// Reply label signalling "no TPM present"
const REPLY_ENODEV: u32 = 1;

// ── TPM2 command templates ──────────────────────────────────────────────

// TPM2_Startup(SU_CLEAR)
const CMD_STARTUP: [u8; 12] = [
    0x80, 0x01,                         // TPM_ST_NO_SESSIONS
    0x00, 0x00, 0x00, 0x0C,             // size = 12
    0x00, 0x00, 0x01, 0x44,             // CC = TPM2_Startup
    0x00, 0x00,                         // SU_CLEAR
];

// ── Cached state ────────────────────────────────────────────────────────

struct TpmState {
    srk_handle: u32,
    srk_valid: bool,
    aik_handle: Option<u32>,
}

impl TpmState {
    fn new() -> Self {
        Self { srk_handle: 0, srk_valid: false, aik_handle: None }
    }
}

// ── Entry point ─────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("tpmd: fatal error {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    debug_print("tpmd: starting")?;

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let ipc_cap = info.tokens[TOKEN_IPC];

    // Map TPM TIS MMIO page (read/write + device)
    space_map(
        space_token,
        TPM_MMIO_VIRT,
        TPM_MMIO_PHYS,
        MAP_DEVICE | 0x03,
        0,
    )?;

    let mmio = MmioRegion::new(TPM_MMIO_VIRT);

    // Probe DID_VID
    let did_vid = mmio.read_u32(REG_DID_VID);
    let vendor_id = (did_vid & 0xFFFF) as u16;
    let device_id = ((did_vid >> 16) & 0xFFFF) as u16;

    let tpm_present = vendor_id != 0xFFFF && vendor_id != 0x0000;

    if tpm_present {
        debug_print(&format!(
            "tpmd: TPM detected, vendor=0x{:04x} device=0x{:04x}",
            vendor_id, device_id
        ))?;

        // Request locality 0 and send Startup
        request_locality(&mmio)?;
        let mut resp = [0u8; 128];
        tis_send(&mmio, &CMD_STARTUP)?;
        let n = tis_recv(&mmio, &mut resp)?;
        if n >= 10 {
            let rc = parse_response_code(&resp);
            // RC 0x000 = success, RC 0x100 = TPM_RC_INITIALIZE (already started)
            if rc != 0 && rc != 0x100 {
                debug_print(&format!("tpmd: TPM2_Startup failed, RC=0x{:03x}", rc))?;
            }
        }
        release_locality(&mmio);
    } else {
        debug_print("tpmd: no TPM device found, running in stub mode")?;
    }

    // Register with service registry and enter IPC loop
    registry::init("tpmd")?;
    let listen_endpoint = endpoint_create(ipc_cap)?;
    registry::register_output("main", listen_endpoint)?;
    debug_print("tpmd: registered, entering IPC loop")?;

    let registry_endpoint = registry::control_endpoint();
    let mut tpm_state = TpmState::new();

    let mut buf = [0u8; 4096];
    loop {
        let tokens = [listen_endpoint, registry_endpoint];
        match ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                if len < size_of::<Message>() {
                    continue;
                }

                let msg = unsafe { &*(buf.as_ptr() as *const Message) };

                if index == 1 {
                    let _ = registry::handle_incoming_message(msg, &buf[size_of::<Message>()..len]);
                    continue;
                }

                let reply_id = match extract_reply_id(msg) {
                    Some(id) => id,
                    None => continue, // not a call — ignore
                };

                // Extract payload from the receive buffer for seal/unseal
                let payload_slice = if len > size_of::<Message>() {
                    &buf[size_of::<Message>()..len]
                } else {
                    &[] as &[u8]
                };

                handle_request(&mmio, tpm_present, vendor_id, device_id, msg, reply_id, &mut tpm_state, payload_slice);
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}

// ── IPC dispatch ────────────────────────────────────────────────────────

fn handle_request(
    mmio: &MmioRegion,
    tpm_present: bool,
    vendor_id: u16,
    device_id: u16,
    msg: &Message,
    reply_id: usize,
    state: &mut TpmState,
    payload: &[u8],
) {
    match msg.tag.label {
        LABEL_STARTUP       => handle_startup(mmio, tpm_present, reply_id),
        LABEL_PCR_READ      => handle_pcr_read(mmio, tpm_present, msg, reply_id),
        LABEL_PCR_EXTEND    => handle_pcr_extend(mmio, tpm_present, msg, reply_id),
        LABEL_GET_INFO      => handle_get_info(tpm_present, vendor_id, device_id, reply_id),
        LABEL_CREATE_PRIMARY => handle_create_primary(mmio, tpm_present, reply_id, state),
        LABEL_SEAL          => handle_seal(mmio, tpm_present, msg, reply_id, state, payload),
        LABEL_UNSEAL        => handle_unseal(mmio, tpm_present, msg, reply_id, state, payload),
        LABEL_CREATE_AIK    => handle_create_aik(mmio, tpm_present, reply_id, state),
        LABEL_QUOTE         => handle_quote(mmio, tpm_present, reply_id, state, payload),
        _ => {
            let r = Message::new(0, [0; 6], 0);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
    }
}

fn handle_startup(mmio: &MmioRegion, tpm_present: bool, reply_id: usize) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let rc = match do_tpm_command(mmio, &CMD_STARTUP) {
        Ok(resp) => parse_response_code(&resp),
        Err(_) => 0xFFFF,
    };

    let r = Message::new(0, [rc as usize, 0, 0, 0, 0, 0], 1);
    let _ = reply(reply_id, &r, IpcFlags::empty());
}

fn handle_pcr_read(mmio: &MmioRegion, tpm_present: bool, msg: &Message, reply_id: usize) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let pcr_index = msg.words[0] as u32;
    let mut cmd = [0u8; 20];
    build_pcr_read(pcr_index, &mut cmd);

    let (rc, hash) = match do_tpm_command(mmio, &cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            let hash = if rc == 0 { parse_pcr_value(&resp) } else { [0u8; 32] };
            (rc, hash)
        }
        Err(_) => (0xFFFF, [0u8; 32]),
    };

    let mut words = [0usize; 6];
    words[0] = rc as usize;
    pack_hash(&hash, &mut words[1..5]);
    // words[5] stays 0
    let r = Message::new(0, words, 5);
    let _ = reply(reply_id, &r, IpcFlags::empty());
}

fn handle_pcr_extend(mmio: &MmioRegion, tpm_present: bool, msg: &Message, reply_id: usize) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let pcr_index = msg.words[0] as u32;
    let digest = unpack_hash(&msg.words[1..5]);
    let mut cmd = [0u8; 65];
    build_pcr_extend(pcr_index, &digest, &mut cmd);

    let rc = match do_tpm_command(mmio, &cmd) {
        Ok(resp) => parse_response_code(&resp),
        Err(_) => 0xFFFF,
    };

    let r = Message::new(0, [rc as usize, 0, 0, 0, 0, 0], 1);
    let _ = reply(reply_id, &r, IpcFlags::empty());
}

fn handle_get_info(tpm_present: bool, vendor_id: u16, device_id: u16, reply_id: usize) {
    let r = Message::new(
        0,
        [
            vendor_id as usize,
            device_id as usize,
            tpm_present as usize,
            0,
            0,
            0,
        ],
        3,
    );
    let _ = reply(reply_id, &r, IpcFlags::empty());
}

// ── Seal/Unseal handlers ────────────────────────────────────────────────

fn handle_create_primary(mmio: &MmioRegion, tpm_present: bool, reply_id: usize, state: &mut TpmState) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let cmd = build_create_primary();
    match do_tpm_command_large(mmio, &cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc == 0 && resp.len() >= 14 {
                state.srk_handle = u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]]);
                state.srk_valid = true;
            }
            let r = Message::new(0, [rc as usize, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
        Err(_) => {
            let r = Message::new(0, [0xFFFF, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
    }
}

fn handle_seal(
    mmio: &MmioRegion,
    tpm_present: bool,
    msg: &Message,
    reply_id: usize,
    state: &mut TpmState,
    payload: &[u8],
) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    if !state.srk_valid {
        let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1); // error: no SRK
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let data_len = msg.words[0];
    if data_len == 0 || data_len > payload.len() || data_len > 128 {
        let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }
    let plaintext = &payload[..data_len];

    // Read PCR 9 and PCR 14 internally via TIS
    let pcr9_val = match read_pcr_internal(mmio, 9) {
        Some(v) => v,
        None => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
            return;
        }
    };
    let pcr14_val = match read_pcr_internal(mmio, 14) {
        Some(v) => v,
        None => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
            return;
        }
    };

    let policy_digest = compute_policy_digest(&pcr9_val, &pcr14_val);
    let cmd = build_tpm2_create(state.srk_handle, plaintext, &policy_digest);

    match do_tpm_command_large(mmio, &cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc != 0 {
                let r = Message::new(0, [rc as usize, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }

            // Parse sealed blob from response:
            // bytes 10..14 = parameterSize
            // then outPrivate (2-byte size + data), then outPublic (2-byte size + data)
            if resp.len() < 16 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            let param_size = u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]]) as usize;
            let blob_start = 14;
            let blob_end = blob_start + param_size;
            if blob_end > resp.len() {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            let sealed_blob = &resp[blob_start..blob_end];

            let r = Message::new(0, [0, sealed_blob.len(), 0, 0, 0, 0], 2);
            let _ = reply_with_payload(reply_id, &r, sealed_blob);
        }
        Err(_) => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
    }
}

fn handle_unseal(
    mmio: &MmioRegion,
    tpm_present: bool,
    msg: &Message,
    reply_id: usize,
    state: &mut TpmState,
    payload: &[u8],
) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    if !state.srk_valid {
        let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let blob_len = msg.words[0];
    if blob_len == 0 || blob_len > payload.len() {
        let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }
    let sealed_blob = &payload[..blob_len];

    // Step 1: TPM2_Load — split blob into outPrivate + outPublic
    let priv_size = if sealed_blob.len() >= 2 {
        u16::from_be_bytes([sealed_blob[0], sealed_blob[1]]) as usize
    } else {
        let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    };
    let priv_total = 2 + priv_size;
    if priv_total >= sealed_blob.len() {
        let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }
    let in_private = &sealed_blob[..priv_total];
    let in_public = &sealed_blob[priv_total..];

    let load_cmd = build_tpm2_load(state.srk_handle, in_private, in_public);
    let sealed_handle = match do_tpm_command_large(mmio, &load_cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc != 0 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            if resp.len() < 14 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]])
        }
        Err(_) => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
            return;
        }
    };

    // Step 2: TPM2_StartAuthSession
    let session_cmd = build_start_auth_session();
    let session_handle = match do_tpm_command_large(mmio, &session_cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc != 0 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            if resp.len() < 14 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]])
        }
        Err(_) => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
            return;
        }
    };

    // Step 3: TPM2_PolicyPCR
    let policy_cmd = build_policy_pcr(session_handle);
    match do_tpm_command_large(mmio, &policy_cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc != 0 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
        }
        Err(_) => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
            return;
        }
    }

    // Step 4: TPM2_Unseal
    let unseal_cmd = build_tpm2_unseal(sealed_handle, session_handle);
    match do_tpm_command_large(mmio, &unseal_cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc == 0x0000_099D {
                // TPM_RC_POLICY_FAIL
                let r = Message::new(0, [1, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            if rc != 0 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }

            // Parse unseal response: parameterSize at 10..14,
            // outData.size at 14..16, outData.buffer at 16+
            if resp.len() < 16 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            let out_data_size = u16::from_be_bytes([resp[14], resp[15]]) as usize;
            let data_end = 16 + out_data_size;
            if data_end > resp.len() {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            let unsealed = &resp[16..data_end];

            let r = Message::new(0, [0, unsealed.len(), 0, 0, 0, 0], 2);
            let _ = reply_with_payload(reply_id, &r, unsealed);
        }
        Err(_) => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
    }
}

// ── AIK + Quote handlers ─────────────────────────────────────────────────

fn handle_create_aik(mmio: &MmioRegion, tpm_present: bool, reply_id: usize, state: &mut TpmState) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let cmd = build_create_aik();
    match do_tpm_command_large(mmio, &cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc == 0 && resp.len() >= 14 {
                let handle = u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]]);
                state.aik_handle = Some(handle);
            }
            let r = Message::new(0, [rc as usize, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
        Err(_) => {
            let r = Message::new(0, [0xFFFF, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
    }
}

fn handle_quote(
    mmio: &MmioRegion,
    tpm_present: bool,
    reply_id: usize,
    state: &TpmState,
    payload: &[u8],
) {
    if !tpm_present {
        let r = Message::new(REPLY_ENODEV, [0; 6], 0);
        let _ = reply(reply_id, &r, IpcFlags::empty());
        return;
    }

    let aik = match state.aik_handle {
        Some(h) => h,
        None => {
            let r = Message::new(0, [3, 0, 0, 0, 0, 0], 1); // no AIK
            let _ = reply(reply_id, &r, IpcFlags::empty());
            return;
        }
    };

    // Caller sends 32-byte nonce in payload
    let mut nonce = [0u8; 32];
    let copy_len = payload.len().min(32);
    nonce[..copy_len].copy_from_slice(&payload[..copy_len]);

    let cmd = build_tpm2_quote(aik, &nonce);
    match do_tpm_command_large(mmio, &cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc != 0 {
                let r = Message::new(0, [rc as usize, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }

            // Extract parameterSize, then quoted + signature
            if resp.len() < 14 {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            let param_size = u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]]) as usize;
            let data_end = 14 + param_size;
            if data_end > resp.len() {
                let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
                let _ = reply(reply_id, &r, IpcFlags::empty());
                return;
            }
            let quote_data = &resp[14..data_end];

            let r = Message::new(0, [0, param_size, 0, 0, 0, 0], 2);
            let _ = reply_with_payload(reply_id, &r, quote_data);
        }
        Err(_) => {
            let r = Message::new(0, [2, 0, 0, 0, 0, 0], 1);
            let _ = reply(reply_id, &r, IpcFlags::empty());
        }
    }
}

// ── Internal PCR read (used by seal) ────────────────────────────────────

fn read_pcr_internal(mmio: &MmioRegion, pcr: u32) -> Option<[u8; 32]> {
    let mut cmd = [0u8; 20];
    build_pcr_read(pcr, &mut cmd);
    match do_tpm_command(mmio, &cmd) {
        Ok(resp) => {
            let rc = parse_response_code(&resp);
            if rc == 0 { Some(parse_pcr_value(&resp)) } else { None }
        }
        Err(_) => None,
    }
}

// ── Policy digest computation ───────────────────────────────────────────

fn compute_policy_digest(pcr9_val: &[u8; 32], pcr14_val: &[u8; 32]) -> [u8; 32] {
    // Hash(PCR9 || PCR14) to get the composite PCR digest
    let mut pcr_concat = [0u8; 64];
    pcr_concat[..32].copy_from_slice(pcr9_val);
    pcr_concat[32..].copy_from_slice(pcr14_val);
    let pcr_digest = sha256(&pcr_concat);

    // PolicyPCR trial: SHA256(old_policy || CC_PolicyPCR || pcr_digest || pcr_selection)
    // old_policy = 32 zero bytes (initial policy)
    let mut input = [0u8; 78]; // 32 + 4 + 32 + 10
    // input[0..32] = 0 (initial empty policy)
    input[32..36].copy_from_slice(&0x0000_017Fu32.to_be_bytes()); // CC_PolicyPCR
    input[36..68].copy_from_slice(&pcr_digest);
    // PCR selection: count=1, hash=SHA256(0x000B), sizeOfSelect=3, pcrSelect=[0x00,0x42,0x00]
    input[68..78].copy_from_slice(&[0x00,0x00,0x00,0x01, 0x00,0x0B, 0x03, 0x00,0x42,0x00]);
    sha256(&input)
}

// ── TPM2 command builders ───────────────────────────────────────────────

fn build_pcr_read(pcr: u32, buf: &mut [u8; 20]) {
    *buf = [0u8; 20];
    // TPM_ST_NO_SESSIONS
    buf[0] = 0x80; buf[1] = 0x01;
    // Size = 20
    let size: u32 = 20;
    buf[2..6].copy_from_slice(&size.to_be_bytes());
    // CC = TPM2_PCR_Read (0x0000017E)
    buf[6..10].copy_from_slice(&0x0000_017Eu32.to_be_bytes());
    // TPML_PCR_SELECTION: count = 1
    buf[10..14].copy_from_slice(&1u32.to_be_bytes());
    // TPMS_PCR_SELECTION: hash = SHA-256 (0x000B), sizeofSelect = 3
    buf[14] = 0x00; buf[15] = 0x0B;
    buf[16] = 3;
    // PCR bitmask (3 bytes, little-endian bitmap)
    let byte_idx = (pcr / 8) as usize;
    let bit_idx = pcr % 8;
    if byte_idx < 3 {
        buf[17 + byte_idx] = 1 << bit_idx;
    }
}

fn build_pcr_extend(pcr: u32, digest: &[u8; 32], buf: &mut [u8; 65]) {
    *buf = [0u8; 65];
    // TPM_ST_SESSIONS (auth required)
    buf[0] = 0x80; buf[1] = 0x02;
    // Total size = 65
    let size: u32 = 65;
    buf[2..6].copy_from_slice(&size.to_be_bytes());
    // CC = TPM2_PCR_Extend (0x00000182)
    buf[6..10].copy_from_slice(&0x0000_0182u32.to_be_bytes());
    // PCR handle
    buf[10..14].copy_from_slice(&pcr.to_be_bytes());
    // Authorization size (u32) = 13 (password session, empty nonce/hmac)
    buf[14..18].copy_from_slice(&13u32.to_be_bytes());
    // Auth: TPM_RS_PW (0x40000009)
    buf[18..22].copy_from_slice(&0x4000_0009u32.to_be_bytes());
    // Nonce size = 0
    buf[22..24].copy_from_slice(&0u16.to_be_bytes());
    // Session attributes = 0
    buf[24] = 0;
    // HMAC size = 0
    buf[25..27].copy_from_slice(&0u16.to_be_bytes());
    // TPML_DIGEST_VALUES: count = 1
    buf[27..31].copy_from_slice(&1u32.to_be_bytes());
    // TPMT_HA: hash = SHA-256 (0x000B)
    buf[31] = 0x00; buf[32] = 0x0B;
    // Digest (32 bytes)
    buf[33..65].copy_from_slice(digest);
}

/// TPM2_CreatePrimary — RSA-2048 AIK (restricted signing key) under owner hierarchy.
fn build_create_aik() -> [u8; 65] {
    let mut cmd = [0u8; 65];
    let mut off = 0;

    // Header
    cmd[off] = 0x80; cmd[off+1] = 0x02; off += 2; // TPM_ST_SESSIONS
    // size filled at end
    off += 4;
    cmd[6..10].copy_from_slice(&0x0000_0131u32.to_be_bytes()); off = 10; // CC_CreatePrimary

    // primaryHandle = TPM_RH_OWNER
    cmd[off..off+4].copy_from_slice(&0x4000_0001u32.to_be_bytes()); off += 4;

    // authorizationSize = 9
    cmd[off..off+4].copy_from_slice(&9u32.to_be_bytes()); off += 4;
    // Auth area: TPM_RS_PW, nonce.size=0, attrs=0x01, hmac.size=0
    cmd[off..off+4].copy_from_slice(&0x4000_0009u32.to_be_bytes()); off += 4;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // nonce.size=0
    cmd[off] = 0x01; off += 1; // sessionAttributes = continueSession
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // hmac.size=0
    // off = 27

    // inSensitive: size=4, userAuth.size=0, data.size=0
    cmd[off..off+2].copy_from_slice(&4u16.to_be_bytes()); off += 2;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // userAuth.size=0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // data.size=0
    // off = 33

    // inPublic template (24 bytes):
    // type(2)+nameAlg(2)+objAttrs(4)+authPolicy.size(2)+sym.alg(2)+
    // scheme.scheme(2)+scheme.hashAlg(2)+keyBits(2)+exponent(4)+unique.size(2) = 24
    let template_size: u16 = 24;
    cmd[off..off+2].copy_from_slice(&template_size.to_be_bytes()); off += 2;
    // type = TPM_ALG_RSA (0x0001)
    cmd[off] = 0x00; cmd[off+1] = 0x01; off += 2;
    // nameAlg = SHA-256 (0x000B)
    cmd[off] = 0x00; cmd[off+1] = 0x0B; off += 2;
    // objectAttributes = 0x00050072
    // fixedTPM|fixedParent|sensitiveDataOrigin|userWithAuth|restricted|sign
    cmd[off..off+4].copy_from_slice(&0x0005_0072u32.to_be_bytes()); off += 4;
    // authPolicy.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // symmetric: NULL (signing key — no inner symmetric)
    cmd[off] = 0x00; cmd[off+1] = 0x10; off += 2;
    // scheme = RSASSA (0x0014)
    cmd[off] = 0x00; cmd[off+1] = 0x14; off += 2;
    // scheme.hashAlg = SHA-256 (0x000B)
    cmd[off] = 0x00; cmd[off+1] = 0x0B; off += 2;
    // keyBits = 2048 (0x0800)
    cmd[off] = 0x08; cmd[off+1] = 0x00; off += 2;
    // exponent = 0 (default 65537)
    cmd[off..off+4].copy_from_slice(&0u32.to_be_bytes()); off += 4;
    // unique.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // off = 59

    // outsideInfo.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // creationPCR.count = 0
    cmd[off..off+4].copy_from_slice(&0u32.to_be_bytes()); off += 4;
    // off = 65

    debug_assert_eq!(off, 65);
    cmd[2..6].copy_from_slice(&(65u32).to_be_bytes());
    cmd
}

/// TPM2_Quote — sign PCR 9 + 14 with AIK.
fn build_tpm2_quote(aik_handle: u32, nonce: &[u8; 32]) -> [u8; 73] {
    let mut cmd = [0u8; 73];
    let mut off = 0;

    // Header
    cmd[off] = 0x80; cmd[off+1] = 0x02; off += 2; // TPM_ST_SESSIONS
    off += 4; // size filled at end
    cmd[6..10].copy_from_slice(&0x0000_0158u32.to_be_bytes()); off = 10; // CC_Quote

    // signHandle = AIK
    cmd[off..off+4].copy_from_slice(&aik_handle.to_be_bytes()); off += 4;

    // authorizationSize = 9
    cmd[off..off+4].copy_from_slice(&9u32.to_be_bytes()); off += 4;
    cmd[off..off+4].copy_from_slice(&0x4000_0009u32.to_be_bytes()); off += 4; // TPM_RS_PW
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // nonce.size=0
    cmd[off] = 0x01; off += 1; // attrs = continueSession
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // hmac.size=0
    // off = 27

    // qualifyingData.size = 32
    cmd[off..off+2].copy_from_slice(&32u16.to_be_bytes()); off += 2;
    cmd[off..off+32].copy_from_slice(nonce); off += 32;
    // off = 61

    // inScheme = NULL (use key's default RSASSA-SHA256)
    cmd[off] = 0x00; cmd[off+1] = 0x10; off += 2;

    // PCRselect: count=1, hash=SHA256, sizeOfSelect=3, PCR 9 + PCR 14
    cmd[off..off+4].copy_from_slice(&1u32.to_be_bytes()); off += 4;
    cmd[off] = 0x00; cmd[off+1] = 0x0B; off += 2; // SHA256
    cmd[off] = 0x03; off += 1; // sizeOfSelect = 3
    cmd[off] = 0x00; off += 1; // byte 0: no PCRs
    cmd[off] = 0x42; off += 1; // byte 1: PCR 9 (bit 1) + PCR 14 (bit 6)
    cmd[off] = 0x00; off += 1; // byte 2: no PCRs
    // off = 73

    debug_assert_eq!(off, 73);
    cmd[2..6].copy_from_slice(&(73u32).to_be_bytes());
    cmd
}

/// TPM2_CreatePrimary — RSA-2048 SRK under owner hierarchy.
fn build_create_primary() -> [u8; 67] {
    let mut cmd = [0u8; 67];
    let mut off = 0;

    // Header
    cmd[off] = 0x80; cmd[off+1] = 0x02; off += 2; // TPM_ST_SESSIONS
    // size filled at end
    off += 4;
    cmd[6..10].copy_from_slice(&0x0000_0131u32.to_be_bytes()); off = 10; // CC_CreatePrimary

    // primaryHandle = TPM_RH_OWNER
    cmd[off..off+4].copy_from_slice(&0x4000_0001u32.to_be_bytes()); off += 4;

    // authorizationSize = 9
    cmd[off..off+4].copy_from_slice(&9u32.to_be_bytes()); off += 4;
    // Auth area: TPM_RS_PW, nonce.size=0, attrs=0x01, hmac.size=0
    cmd[off..off+4].copy_from_slice(&0x4000_0009u32.to_be_bytes()); off += 4;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // nonce.size=0
    cmd[off] = 0x01; off += 1; // sessionAttributes = continueSession
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // hmac.size=0
    // off = 27

    // inSensitive: size=4, userAuth.size=0, data.size=0
    cmd[off..off+2].copy_from_slice(&4u16.to_be_bytes()); off += 2; // inSensitive.size = 4 (inner)
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // userAuth.size=0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // data.size=0
    // off = 33

    // inPublic template:
    // type(2)+nameAlg(2)+objAttrs(4)+authPolicy.size(2)+sym.alg(2)+sym.bits(2)+sym.mode(2)+scheme(2)+keyBits(2)+exponent(4)+unique.size(2) = 26
    let template_size: u16 = 26;
    cmd[off..off+2].copy_from_slice(&template_size.to_be_bytes()); off += 2; // inPublic.size
    // type = TPM_ALG_RSA (0x0001)
    cmd[off] = 0x00; cmd[off+1] = 0x01; off += 2;
    // nameAlg = SHA-256 (0x000B)
    cmd[off] = 0x00; cmd[off+1] = 0x0B; off += 2;
    // objectAttributes = 0x00030472
    cmd[off..off+4].copy_from_slice(&0x0003_0472u32.to_be_bytes()); off += 4;
    // authPolicy.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // symmetric: AES-128-CFB
    cmd[off] = 0x00; cmd[off+1] = 0x06; off += 2; // AES
    cmd[off] = 0x00; cmd[off+1] = 0x80; off += 2; // 128 bits
    cmd[off] = 0x00; cmd[off+1] = 0x43; off += 2; // CFB
    // scheme = NULL (0x0010)
    cmd[off] = 0x00; cmd[off+1] = 0x10; off += 2;
    // keyBits = 2048 (0x0800)
    cmd[off] = 0x08; cmd[off+1] = 0x00; off += 2;
    // exponent = 0 (default 65537)
    cmd[off..off+4].copy_from_slice(&0u32.to_be_bytes()); off += 4;
    // unique.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // off = 61

    // outsideInfo.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // creationPCR.count = 0
    cmd[off..off+4].copy_from_slice(&0u32.to_be_bytes()); off += 4;
    // off = 67

    debug_assert_eq!(off, 67);
    // Fill in total size
    cmd[2..6].copy_from_slice(&(67u32).to_be_bytes());
    cmd
}

/// TPM2_Create — seal plaintext under SRK, bound to policy_digest.
fn build_tpm2_create(srk_handle: u32, plaintext: &[u8], policy_digest: &[u8; 32]) -> alloc::vec::Vec<u8> {
    // Variable-length command due to plaintext size
    // Base sizes:
    // header(10) + parentHandle(4) + authorizationSize(4) + auth(9) = 27
    // inSensitive: size(2) + userAuth.size(2) + data.size(2) + data = 6 + plaintext.len()
    // inPublic: size(2) + template = variable
    // outsideInfo.size(2) + creationPCR.count(4) = 6
    let in_sensitive_inner = 2 + 2 + plaintext.len(); // userAuth.size + data.size + data
    let in_sensitive_total = 2 + in_sensitive_inner; // size field + inner

    // Template: type(2) + nameAlg(2) + objAttrs(4) + authPolicy.size(2) + authPolicy(32) +
    //           scheme(2) + unique.size(2) = 46
    let template_size: usize = 2 + 2 + 4 + 2 + 32 + 2 + 2;
    let in_public_total = 2 + template_size;

    let total_size = 10 + 4 + 4 + 9 + in_sensitive_total + in_public_total + 2 + 4;
    let mut cmd = alloc::vec![0u8; total_size];

    let mut off = 0;
    // Header
    cmd[off] = 0x80; cmd[off+1] = 0x02; off += 2; // TPM_ST_SESSIONS
    cmd[off..off+4].copy_from_slice(&(total_size as u32).to_be_bytes()); off += 4;
    cmd[off..off+4].copy_from_slice(&0x0000_0153u32.to_be_bytes()); off += 4; // CC_Create

    // parentHandle
    cmd[off..off+4].copy_from_slice(&srk_handle.to_be_bytes()); off += 4;

    // authorizationSize = 9
    cmd[off..off+4].copy_from_slice(&9u32.to_be_bytes()); off += 4;
    // Auth: TPM_RS_PW
    cmd[off..off+4].copy_from_slice(&0x4000_0009u32.to_be_bytes()); off += 4;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // nonce.size=0
    cmd[off] = 0x01; off += 1; // attrs = continueSession
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // hmac.size=0

    // inSensitive
    cmd[off..off+2].copy_from_slice(&(in_sensitive_inner as u16).to_be_bytes()); off += 2;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2; // userAuth.size=0
    cmd[off..off+2].copy_from_slice(&(plaintext.len() as u16).to_be_bytes()); off += 2;
    cmd[off..off+plaintext.len()].copy_from_slice(plaintext); off += plaintext.len();

    // inPublic
    cmd[off..off+2].copy_from_slice(&(template_size as u16).to_be_bytes()); off += 2;
    // type = KEYEDHASH (0x0008)
    cmd[off] = 0x00; cmd[off+1] = 0x08; off += 2;
    // nameAlg = SHA256
    cmd[off] = 0x00; cmd[off+1] = 0x0B; off += 2;
    // objectAttributes = 0x00000062 (fixedTPM|fixedParent, NO sensitiveDataOrigin)
    cmd[off..off+4].copy_from_slice(&0x0000_0062u32.to_be_bytes()); off += 4;
    // authPolicy.size = 32
    cmd[off..off+2].copy_from_slice(&32u16.to_be_bytes()); off += 2;
    cmd[off..off+32].copy_from_slice(policy_digest); off += 32;
    // scheme = NULL (0x0010)
    cmd[off] = 0x00; cmd[off+1] = 0x10; off += 2;
    // unique.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;

    // outsideInfo.size = 0
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    // creationPCR.count = 0
    cmd[off..off+4].copy_from_slice(&0u32.to_be_bytes()); off += 4;

    debug_assert_eq!(off, total_size);
    cmd
}

/// TPM2_Load — load sealed object under SRK.
fn build_tpm2_load(srk_handle: u32, in_private: &[u8], in_public: &[u8]) -> alloc::vec::Vec<u8> {
    let total_size = 10 + 4 + 4 + 9 + in_private.len() + in_public.len();
    let mut cmd = alloc::vec![0u8; total_size];

    let mut off = 0;
    cmd[off] = 0x80; cmd[off+1] = 0x02; off += 2; // TPM_ST_SESSIONS
    cmd[off..off+4].copy_from_slice(&(total_size as u32).to_be_bytes()); off += 4;
    cmd[off..off+4].copy_from_slice(&0x0000_0157u32.to_be_bytes()); off += 4; // CC_Load

    cmd[off..off+4].copy_from_slice(&srk_handle.to_be_bytes()); off += 4;

    // authorizationSize = 9
    cmd[off..off+4].copy_from_slice(&9u32.to_be_bytes()); off += 4;
    cmd[off..off+4].copy_from_slice(&0x4000_0009u32.to_be_bytes()); off += 4;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;
    cmd[off] = 0x01; off += 1;
    cmd[off..off+2].copy_from_slice(&0u16.to_be_bytes()); off += 2;

    // inPrivate (already includes 2-byte size prefix)
    cmd[off..off+in_private.len()].copy_from_slice(in_private); off += in_private.len();
    // inPublic (already includes 2-byte size prefix)
    cmd[off..off+in_public.len()].copy_from_slice(in_public); off += in_public.len();

    debug_assert_eq!(off, total_size);
    cmd
}

/// TPM2_StartAuthSession — policy session, SHA-256.
fn build_start_auth_session() -> [u8; 41] {
    let mut cmd = [0u8; 41];
    let size: u32 = 41;

    cmd[0] = 0x80; cmd[1] = 0x01; // TPM_ST_NO_SESSIONS
    cmd[2..6].copy_from_slice(&size.to_be_bytes());
    cmd[6..10].copy_from_slice(&0x0000_0176u32.to_be_bytes()); // CC_StartAuthSession

    // tpmKey = TPM_RH_NULL
    cmd[10..14].copy_from_slice(&0x4000_0007u32.to_be_bytes());
    // bind = TPM_RH_NULL
    cmd[14..18].copy_from_slice(&0x4000_0007u32.to_be_bytes());

    // nonceCaller: size=16, then 16 bytes from RDRAND
    cmd[18..20].copy_from_slice(&16u16.to_be_bytes());
    let nonce = rdrand_16();
    cmd[20..36].copy_from_slice(&nonce);

    // sessionType = TPM_SE_POLICY (0x01)
    cmd[36] = 0x01;

    // symmetric.algorithm = NULL (0x0010)
    cmd[37] = 0x00; cmd[38] = 0x10;

    // authHashAlg = SHA-256 (0x000B)
    cmd[39] = 0x00; cmd[40] = 0x0B;

    cmd
}

/// TPM2_PolicyPCR — bind session to PCR 9 + 14.
fn build_policy_pcr(session_handle: u32) -> [u8; 26] {
    let mut cmd = [0u8; 26];
    let size: u32 = 26;

    cmd[0] = 0x80; cmd[1] = 0x01; // TPM_ST_NO_SESSIONS
    cmd[2..6].copy_from_slice(&size.to_be_bytes());
    cmd[6..10].copy_from_slice(&0x0000_017Fu32.to_be_bytes()); // CC_PolicyPCR

    cmd[10..14].copy_from_slice(&session_handle.to_be_bytes());

    // pcrDigest.size = 0 (empty = TPM reads current PCR values)
    cmd[14..16].copy_from_slice(&0u16.to_be_bytes());

    // TPML_PCR_SELECTION: count=1
    cmd[16..20].copy_from_slice(&1u32.to_be_bytes());
    // hash = SHA256
    cmd[20] = 0x00; cmd[21] = 0x0B;
    // sizeOfSelect = 3
    cmd[22] = 0x03;
    // pcrSelect: PCR 9 (byte1 bit1) + PCR 14 (byte1 bit6) = 0x42
    cmd[23] = 0x00;
    cmd[24] = 0x42;
    cmd[25] = 0x00;

    cmd
}

/// TPM2_Unseal
fn build_tpm2_unseal(item_handle: u32, session_handle: u32) -> [u8; 27] {
    let mut cmd = [0u8; 27];
    // header(10) + itemHandle(4) + authorizationSize(4) + auth(9) = 27
    let size: u32 = 27;

    cmd[0] = 0x80; cmd[1] = 0x02; // TPM_ST_SESSIONS
    cmd[2..6].copy_from_slice(&size.to_be_bytes());
    cmd[6..10].copy_from_slice(&0x0000_015Eu32.to_be_bytes()); // CC_Unseal

    cmd[10..14].copy_from_slice(&item_handle.to_be_bytes());

    // authorizationSize = 9
    cmd[14..18].copy_from_slice(&9u32.to_be_bytes());
    // Auth: session_handle (policy session), nonce.size=0, attrs=0x01, hmac.size=0
    cmd[18..22].copy_from_slice(&session_handle.to_be_bytes());
    cmd[22..24].copy_from_slice(&0u16.to_be_bytes());
    cmd[24] = 0x01;
    cmd[25..27].copy_from_slice(&0u16.to_be_bytes());

    cmd
}

fn parse_response_code(resp: &[u8]) -> u32 {
    if resp.len() < 10 {
        return 0xFFFF;
    }
    u32::from_be_bytes([resp[6], resp[7], resp[8], resp[9]])
}

fn parse_pcr_value(resp: &[u8]) -> [u8; 32] {
    let mut hash = [0u8; 32];
    // PCR_Read response: digest starts at offset 30 (after headers + pcr update counter + selection)
    if resp.len() >= 62 {
        hash.copy_from_slice(&resp[30..62]);
    }
    hash
}

// ── Hash pack/unpack for IPC words ──────────────────────────────────────

fn pack_hash(hash: &[u8; 32], words: &mut [usize]) {
    words[0] = usize::from_be_bytes(hash[0..8].try_into().unwrap_or([0; 8]));
    words[1] = usize::from_be_bytes(hash[8..16].try_into().unwrap_or([0; 8]));
    words[2] = usize::from_be_bytes(hash[16..24].try_into().unwrap_or([0; 8]));
    words[3] = usize::from_be_bytes(hash[24..32].try_into().unwrap_or([0; 8]));
}

fn unpack_hash(words: &[usize]) -> [u8; 32] {
    let mut hash = [0u8; 32];
    if words.len() >= 4 {
        hash[0..8].copy_from_slice(&words[0].to_be_bytes());
        hash[8..16].copy_from_slice(&words[1].to_be_bytes());
        hash[16..24].copy_from_slice(&words[2].to_be_bytes());
        hash[24..32].copy_from_slice(&words[3].to_be_bytes());
    }
    hash
}

// ── RDRAND helper ───────────────────────────────────────────────────────

fn rdrand_16() -> [u8; 16] {
    let mut buf = [0u8; 16];
    let mut r: u64;
    let mut ok: u8;

    r = 0;
    for _ in 0..10 {
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {ok}",
                val = out(reg) r,
                ok = out(reg_byte) ok,
            );
        }
        if ok != 0 { break; }
    }
    buf[..8].copy_from_slice(&r.to_le_bytes());

    r = 0;
    for _ in 0..10 {
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {ok}",
                val = out(reg) r,
                ok = out(reg_byte) ok,
            );
        }
        if ok != 0 { break; }
    }
    buf[8..].copy_from_slice(&r.to_le_bytes());

    buf
}

// ── TIS register helpers ────────────────────────────────────────────────

fn request_locality(mmio: &MmioRegion) -> Result<()> {
    mmio.write_u8(REG_ACCESS, ACCESS_REQUEST);
    for _ in 0..TIMEOUT {
        let val = mmio.read_u8(REG_ACCESS);
        if val & ACCESS_ACTIVE != 0 {
            return Ok(());
        }
    }
    Err(libcluu::Error::Timeout)
}

fn release_locality(mmio: &MmioRegion) {
    mmio.write_u8(REG_ACCESS, ACCESS_ACTIVE);
}

fn read_burst_count(mmio: &MmioRegion) -> Result<usize> {
    for _ in 0..TIMEOUT {
        let sts = mmio.read_u32(REG_STS);
        let burst = ((sts >> 8) & 0xFFFF) as usize;
        if burst > 0 {
            return Ok(burst);
        }
    }
    Err(libcluu::Error::Timeout)
}

fn wait_for_sts(mmio: &MmioRegion, mask: u32) -> Result<()> {
    for _ in 0..TIMEOUT {
        let sts = mmio.read_u32(REG_STS);
        if (sts & STS_VALID) != 0 && (sts & mask) == mask {
            return Ok(());
        }
    }
    Err(libcluu::Error::Timeout)
}

fn tis_send(mmio: &MmioRegion, cmd: &[u8]) -> Result<()> {
    // Signal command ready
    mmio.write_u32(REG_STS, STS_CMD_READY);
    wait_for_sts(mmio, STS_CMD_READY)?;

    // Write command bytes respecting burst count
    let mut offset = 0;
    while offset < cmd.len() {
        let burst = read_burst_count(mmio)?;
        let chunk = core::cmp::min(burst, cmd.len() - offset);
        for i in 0..chunk {
            mmio.write_u8(REG_DATA_FIFO, cmd[offset + i]);
        }
        offset += chunk;
    }

    // Verify TPM is no longer expecting data
    let sts = mmio.read_u32(REG_STS);
    if sts & STS_EXPECT != 0 {
        mmio.write_u32(REG_STS, STS_CMD_READY);
        return Err(libcluu::Error::InvalidArgument);
    }

    // Execute
    mmio.write_u32(REG_STS, STS_GO);
    Ok(())
}

fn tis_recv(mmio: &MmioRegion, buf: &mut [u8]) -> Result<usize> {
    wait_for_sts(mmio, STS_DATA_AVAIL)?;

    // Read 6-byte header to get total response size
    let header_len = core::cmp::min(6, buf.len());
    for i in 0..header_len {
        let burst = read_burst_count(mmio)?;
        let _ = burst; // header bytes come one at a time
        buf[i] = mmio.read_u8(REG_DATA_FIFO);
    }

    if header_len < 6 {
        mmio.write_u32(REG_STS, STS_CMD_READY);
        return Ok(header_len);
    }

    // Parse total size from header bytes 2-5 (big-endian u32)
    let total = u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]) as usize;
    let to_read = core::cmp::min(total, buf.len());

    // Read remaining bytes
    let mut offset = 6;
    while offset < to_read {
        let burst = read_burst_count(mmio)?;
        let chunk = core::cmp::min(burst, to_read - offset);
        for i in 0..chunk {
            buf[offset + i] = mmio.read_u8(REG_DATA_FIFO);
        }
        offset += chunk;
    }

    // Return to ready state
    mmio.write_u32(REG_STS, STS_CMD_READY);
    Ok(offset)
}

/// Send a command and receive the response (128-byte buffer), handling locality.
fn do_tpm_command(mmio: &MmioRegion, cmd: &[u8]) -> core::result::Result<[u8; 128], i32> {
    if request_locality(mmio).is_err() {
        return Err(-1);
    }
    let mut resp = [0u8; 128];
    let ok = tis_send(mmio, cmd).and_then(|()| tis_recv(mmio, &mut resp));
    release_locality(mmio);
    match ok {
        Ok(_) => Ok(resp),
        Err(_) => Err(-1),
    }
}

/// Send a command and receive a larger response (1024 bytes), handling locality.
fn do_tpm_command_large(mmio: &MmioRegion, cmd: &[u8]) -> core::result::Result<alloc::vec::Vec<u8>, i32> {
    if request_locality(mmio).is_err() {
        return Err(-1);
    }
    let mut resp = [0u8; 1024];
    let ok = tis_send(mmio, cmd).and_then(|()| tis_recv(mmio, &mut resp));
    release_locality(mmio);
    match ok {
        Ok(n) => Ok(alloc::vec::Vec::from(&resp[..n])),
        Err(_) => Err(-1),
    }
}
