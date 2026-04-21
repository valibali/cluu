//! Sealed storage PoC — seal/unseal round-trip via tpmd.
//!
//! Best-effort: logs warnings on failure but never halts boot.

use alloc::format;
use libcluu::ipc::{
    call, call_with_reply_buf, TPMD_CREATE_PRIMARY_LABEL, TPMD_SEAL_LABEL, TPMD_UNSEAL_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry};

pub fn run(registry_send: usize, root_token: usize) {
    if registry::init_with_tokens("init-seal", registry_send, root_token).is_err() {
        let _ = debug_print("sealed_storage: registry init failed, skipping");
        return;
    }

    let tpmd_endpoint = match registry::lookup_service("tpmd:main") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("sealed_storage: tpmd not found, skipping");
            return;
        }
    };

    // 1. CreatePrimary (label 5)
    let mut msg = Message::new(TPMD_CREATE_PRIMARY_LABEL, [0; 6], 0);
    if call(tpmd_endpoint, &mut msg, IpcFlags::empty()).is_err() {
        let _ = debug_print("sealed_storage: CreatePrimary IPC failed");
        return;
    }
    if msg.tag.label == 1 {
        return; // stub mode (no TPM), skip silently
    }
    if msg.words[0] != 0 {
        let _ = debug_print(&format!(
            "sealed_storage: CreatePrimary failed rc=0x{:x}",
            msg.words[0]
        ));
        return;
    }
    let _ = debug_print("sealed_storage: SRK created");

    // 2. Generate 32-byte test secret via RDRAND
    let secret = rdrand_32();

    // 3. Seal (label 6) — send plaintext, get back sealed blob
    let seal_msg = Message::new(TPMD_SEAL_LABEL, [32, 0, 0, 0, 0, 0], 1);
    let mut reply_buf = [0u8; 1024];
    let (seal_reply, seal_payload_len) = match call_with_reply_buf(
        tpmd_endpoint,
        &seal_msg,
        &secret,
        &mut reply_buf,
    ) {
        Ok(r) => r,
        Err(_) => {
            let _ = debug_print("sealed_storage: Seal IPC failed");
            return;
        }
    };
    if seal_reply.tag.label == 1 {
        return; // stub mode
    }
    if seal_reply.words[0] != 0 {
        let _ = debug_print(&format!(
            "sealed_storage: Seal failed rc={}",
            seal_reply.words[0]
        ));
        return;
    }
    let blob_len = seal_reply.words[1];
    if blob_len == 0 || blob_len > seal_payload_len {
        let _ = debug_print("sealed_storage: Seal returned invalid blob length");
        return;
    }
    // Payload starts after the Message header in reply_buf
    let msg_size = core::mem::size_of::<Message>();
    let sealed_blob = &reply_buf[msg_size..msg_size + blob_len];
    let _ = debug_print(&format!(
        "sealed_storage: sealed 32 bytes into {} byte blob",
        blob_len
    ));

    // 4. Unseal (label 7) — send sealed blob, get back plaintext
    let unseal_msg = Message::new(TPMD_UNSEAL_LABEL, [blob_len, 0, 0, 0, 0, 0], 1);
    let mut blob_copy = [0u8; 512];
    blob_copy[..blob_len].copy_from_slice(sealed_blob);

    let mut unseal_reply_buf = [0u8; 1024];
    let (unseal_reply, unseal_payload_len) = match call_with_reply_buf(
        tpmd_endpoint,
        &unseal_msg,
        &blob_copy[..blob_len],
        &mut unseal_reply_buf,
    ) {
        Ok(r) => r,
        Err(_) => {
            let _ = debug_print("sealed_storage: Unseal IPC failed");
            return;
        }
    };
    if unseal_reply.words[0] == 1 {
        let _ = debug_print("sealed_storage: Unseal POLICY_FAIL — PCR mismatch");
        return;
    }
    if unseal_reply.words[0] != 0 {
        let _ = debug_print(&format!(
            "sealed_storage: Unseal failed rc={}",
            unseal_reply.words[0]
        ));
        return;
    }
    let data_len = unseal_reply.words[1];
    if data_len == 0 || data_len > unseal_payload_len {
        let _ = debug_print("sealed_storage: Unseal returned invalid data length");
        return;
    }
    let unsealed = &unseal_reply_buf[msg_size..msg_size + data_len];

    // 5. Compare: unsealed must match original secret
    if data_len == 32 && unsealed == &secret[..] {
        let _ = debug_print("sealed_storage: seal/unseal round-trip verified OK");
    } else {
        let _ = debug_print("sealed_storage: MISMATCH — unsealed data differs from original");
    }
}

fn rdrand_32() -> [u8; 32] {
    let mut buf = [0u8; 32];
    for chunk in buf.chunks_mut(8) {
        let mut r: u64 = 0;
        let mut ok: u8 = 0;
        for _ in 0..10 {
            unsafe {
                core::arch::asm!(
                    "rdrand {val}",
                    "setc {ok}",
                    val = out(reg) r,
                    ok = out(reg_byte) ok,
                );
            }
            if ok != 0 {
                break;
            }
        }
        let bytes = r.to_le_bytes();
        let len = chunk.len();
        chunk.copy_from_slice(&bytes[..len]);
    }
    buf
}
