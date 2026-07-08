use alloc::vec::Vec;

pub const MAX_TLS_MODULES: usize = 32;

#[derive(Debug, Clone, Copy, Default)]
pub struct TlsModule {
    pub module_id: u64,
    pub tls_image: u64,
    pub tls_size: u64,
    pub tls_align: u64,
}

#[derive(Debug, Default)]
pub struct TlsBlock {
    pub modules: Vec<TlsModule>,
    pub dtv: Vec<u64>,
}

impl TlsBlock {
    pub fn new() -> Self {
        Self {
            modules: Vec::new(),
            dtv: Vec::new(),
        }
    }

    pub fn register_module(&mut self, module: TlsModule) {
        if self.modules.len() < MAX_TLS_MODULES {
            self.modules.push(module);
        }
    }

    pub fn allocate_dtv(&mut self) {
        self.dtv.clear();
        self.dtv.resize(self.modules.len() + 1, 0);
    }

    pub fn dtv_entry(&self, module_id: u64) -> Option<u64> {
        let idx = module_id as usize;
        if idx > 0 && idx < self.dtv.len() {
            Some(self.dtv[idx])
        } else {
            None
        }
    }

    pub fn set_dtv_entry(&mut self, module_id: u64, addr: u64) {
        let idx = module_id as usize;
        if idx > 0 && idx < self.dtv.len() {
            self.dtv[idx] = addr;
        }
    }
}

pub fn __tls_get_addr(dtv: &mut TlsBlock, module_id: u64, offset: u64) -> u64 {
    if let Some(base) = dtv.dtv_entry(module_id) {
        base + offset
    } else {
        0
    }
}

pub fn init_thread_tls(dtv: &mut TlsBlock, tcb_addr: u64) {
    dtv.allocate_dtv();
    let module_ids: Vec<u64> = dtv.modules.iter().map(|m| m.module_id).collect();
    for (i, module_id) in module_ids.iter().enumerate() {
        let tls_addr = tcb_addr + (i as u64 + 1) * 0x1000;
        dtv.set_dtv_entry(*module_id, tls_addr);
    }
}
