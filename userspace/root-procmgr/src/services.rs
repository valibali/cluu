extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_SERVICE_SPAWN_LABEL;
use procmgr_common::kernel_iface::Kernel;
use crate::dispatch::ProcmgrState;

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub struct ServiceSpawnReq { pub name: String, pub image_path: String }

#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub name: String,
    pub thread_tok: u64,
    pub publish_cap: u64,
    pub restart_policy: crate::restart_root::Policy,
}

pub struct ServiceSpawn;

impl MsgHandler for ServiceSpawn {
    const LABEL: u32 = PROCMGR_SERVICE_SPAWN_LABEL;
    type State = ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: ServiceSpawnReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        let thread_tok = state.kernel.spawn_thread(0xE000_0000, 0xF000_0000);
        let publish_cap = state.kernel.mint(0xBEEF_BEEF, 0xFF);
        state.services.push(ServiceEntry {
            name: req.name, thread_tok, publish_cap,
            restart_policy: crate::restart_root::Policy::Always,
        });
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;

    #[test]
    fn spawn_vfs_records_thread_and_publish_cap() {
        let mut s = ProcmgrState::new_for_test();
        let req = ServiceSpawnReq { name: "vfs".into(), image_path: "/sbin/vfs".into() };
        let p = postcard::to_allocvec(&req).unwrap();
        let msg = InboundMsg { label: ServiceSpawn::LABEL, words: [0; 6], payload: &p, sender_tid: 1 };
        ServiceSpawn::handle(&mut s, &msg).unwrap();
        assert_eq!(s.services.len(), 1);
        assert_eq!(s.services[0].name, "vfs");
        let spawns = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::SpawnThread { .. })).count();
        let mints = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::Mint { .. })).count();
        assert_eq!(spawns, 1);
        assert_eq!(mints, 1);
    }

    #[test]
    fn bad_payload() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg { label: ServiceSpawn::LABEL, words: [0; 6], payload: &[0xFF], sender_tid: 1 };
        assert!(matches!(ServiceSpawn::handle(&mut s, &msg), Err(HandlerError::BadPayload)));
    }
}
