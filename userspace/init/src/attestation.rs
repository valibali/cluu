//! Remote attestation PoC — AIK creation + TPM Quote via tpmd.
//!
//! Best-effort: logs warnings on failure but never halts boot.

use alloc::format;
use libcluu::ipc::{call, call_with_reply_buf, TPMD_CREATE_AIK_LABEL, TPMD_QUOTE_LABEL};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry};

pub fn run(registry_send: usize, root_token: usize) {
    if registry::init_with_tokens("init-attest", registry_send, root_token).is_err() {
        let _ = debug_print("attestation: registry init failed, skipping");
        return;
    }

    let tpmd_endpoint = match registry::lookup_service("tpmd:main") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("attestation: tpmd not found, skipping");
            return;
        }
    };

    // 1. Create AIK (label 8)
    let mut msg = Message::new(TPMD_CREATE_AIK_LABEL, [0; 6], 0);
    if call(tpmd_endpoint, &mut msg, IpcFlags::empty()).is_err() {
        let _ = debug_print("attestation: CreateAIK IPC failed");
        return;
    }
    if msg.tag.label == 1 {
        return; // stub mode (no TPM), skip silently
    }
    if msg.words[0] != 0 {
        let _ = debug_print(&format!(
            "attestation: AIK creation failed rc=0x{:x}",
            msg.words[0]
        ));
        return;
    }
    let _ = debug_print("attestation: AIK created");

    // 2. Generate 32-byte nonce via RDRAND and request quote (label 9)
    let nonce = rdrand_32();
    let quote_msg = Message::new(TPMD_QUOTE_LABEL, [32, 0, 0, 0, 0, 0], 1);
    let mut reply_buf = [0u8; 1024];
    let (quote_reply, _payload_len) = match call_with_reply_buf(
        tpmd_endpoint,
        &quote_msg,
        &nonce,
        &mut reply_buf,
    ) {
        Ok(r) => r,
        Err(_) => {
            let _ = debug_print("attestation: Quote IPC failed");
            return;
        }
    };
    if quote_reply.tag.label == 1 {
        return; // stub mode
    }
    if quote_reply.words[0] != 0 {
        let _ = debug_print(&format!(
            "attestation: Quote failed rc={}",
            quote_reply.words[0]
        ));
        return;
    }
    let quote_len = quote_reply.words[1];
    let _ = debug_print(&format!(
        "attestation: TPM Quote generated ({} bytes)",
        quote_len
    ));
    // In a real system, this quote + signature would be sent to a remote verifier.
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
