//! Primordial seed types — see spec 1 §13.
//!
//! Wire format for `PROCMGR_PRIMORDIAL_SEED_LABEL = 51`. Init sends this
//! one-shot message to procmgr immediately after procmgr's kernel-spawn.
//! Procmgr rejects the call after first success and rejects any caller
//! other than init's pid.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::spawn::{SpawnEnvelope, SpawnError, SpawnReply};

pub const PROCMGR_PRIMORDIAL_SEED_LABEL: u32 = 51;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimordialSeed {
    pub primordials: Vec<SpawnEnvelope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimordialSeedReply {
    /// One result per envelope in the request, in input order.
    pub results: Vec<Result<SpawnReply, SpawnError>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::{SpawnEnvelope, SpawnError, SpawnReply, ViewSource};
    use alloc::{string::String, vec};

    #[test]
    fn primordial_seed_roundtrip() {
        let seed = PrimordialSeed {
            primordials: vec![SpawnEnvelope {
                image: String::from("registry"),
                args: vec![],
                env: vec![],
                view: ViewSource::BootstrapRoot,
                fd_inherit: Vec::new(),
                session: None,
                notify: None,
            }],
        };
        let bytes = postcard::to_allocvec(&seed).expect("serialize");
        let decoded: PrimordialSeed = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded.primordials.len(), 1);
        assert_eq!(decoded.primordials[0].image, "registry");
    }

    #[test]
    fn reply_roundtrip() {
        let reply = PrimordialSeedReply {
            results: vec![
                Ok(SpawnReply { pid: 2, child_thread_token: 0x1000 }),
                Err(SpawnError::ImageNotFound),
            ],
        };
        let bytes = postcard::to_allocvec(&reply).expect("serialize");
        let decoded: PrimordialSeedReply = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded.results.len(), 2);
        match &decoded.results[0] {
            Ok(r) => assert_eq!(r.pid, 2),
            Err(_) => panic!("expected Ok"),
        }
    }
}