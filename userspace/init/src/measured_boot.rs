//! Measured boot — extend TPM PCRs with service hashes via tpmd IPC.
//!
//! Best-effort: logs warnings on failure but never halts boot.

use alloc::format;
use libcluu::ipc::TPMD_PCR_EXTEND_LABEL;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry};

/// PCR assignments.
const PCR_INITRD: u32 = 9;
const PCR_PRIMORDIAL: u32 = 14;

/// Extend TPM PCRs with measured boot data.
///
/// Best-effort: logs warnings on failure but never halts boot.
pub fn extend_measurements(
    registry_send: usize,
    root_token: usize,
    initrd: &[u8],
    service_hashes: &[([u8; 32], &str)],
) {
    if registry::init_with_tokens("init", registry_send, root_token).is_err() {
        let _ = debug_print("measured_boot: registry init failed, skipping");
        return;
    }

    // Look up tpmd — blocks until tpmd registers (natural ordering sync).
    let tpmd_endpoint = match registry::lookup_service("tpmd:main") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("measured_boot: tpmd not found, skipping");
            return;
        }
    };

    // Extend PCR 9 with initrd hash.
    let initrd_hash = klibcluu::crypto::hash_sha256(initrd);
    extend_pcr(tpmd_endpoint, PCR_INITRD, &initrd_hash, "initrd");

    // Extend PCR 14 with each service binary hash.
    for (hash, name) in service_hashes {
        extend_pcr(tpmd_endpoint, PCR_PRIMORDIAL, hash, name);
    }

    let _ = debug_print("measured_boot: all PCR extends complete");
}

fn extend_pcr(tpmd_endpoint: usize, pcr: u32, hash: &[u8; 32], name: &str) {
    let mut words = [0usize; 6];
    words[0] = pcr as usize;
    pack_hash(hash, &mut words[1..5]);

    let mut msg = Message::new(TPMD_PCR_EXTEND_LABEL, words, 5);
    match libcluu::ipc::call(tpmd_endpoint, &mut msg, IpcFlags::empty()) {
        Ok(()) => {
            // After call, msg is overwritten with the reply.
            // Reply label 1 = ENODEV (stub mode, no TPM — not an error).
            if msg.tag.label != 0 && msg.tag.label != 1 {
                let _ = debug_print(&format!(
                    "measured_boot: PCR{} extend failed for {} (label={})",
                    pcr, name, msg.tag.label
                ));
            } else if msg.tag.label == 0 {
                let rc = msg.words[0];
                if rc != 0 {
                    let _ = debug_print(&format!(
                        "measured_boot: PCR{} extend failed for {} (rc={})",
                        pcr, name, rc
                    ));
                }
            }
        }
        Err(_) => {
            let _ = debug_print(&format!(
                "measured_boot: IPC failed for {} PCR{}", name, pcr
            ));
        }
    }
}

fn pack_hash(hash: &[u8; 32], words: &mut [usize]) {
    words[0] = usize::from_be_bytes(hash[0..8].try_into().unwrap_or([0; 8]));
    words[1] = usize::from_be_bytes(hash[8..16].try_into().unwrap_or([0; 8]));
    words[2] = usize::from_be_bytes(hash[16..24].try_into().unwrap_or([0; 8]));
    words[3] = usize::from_be_bytes(hash[24..32].try_into().unwrap_or([0; 8]));
}
