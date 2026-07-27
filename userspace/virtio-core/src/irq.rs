//! Wrap `irq_attach` into a wait-for-completion primitive.
//!
//! On construction, allocate a private endpoint and call `irq_attach` so
//! the kernel pushes IRQ events as IPC messages to that endpoint. The
//! driver's main recv loop integrates the endpoint into its `recv_any`
//! token list — when an IRQ fires the loop wakes, reads ISR (the caller
//! does this), and drains the used ring.

use libcluu::syscall::{endpoint_create, irq_ack, irq_attach};
use libcluu::Result;

pub struct IrqSource {
    pub endpoint: usize,
    pub irq_number: usize,
    irq_token: usize,
}

impl IrqSource {
    pub fn new(ipc_token: usize, irq_token: usize, irq_number: usize) -> Result<Self> {
        let endpoint = endpoint_create(ipc_token)?;
        irq_attach(irq_token, endpoint, irq_number)?;
        Ok(Self {
            endpoint,
            irq_number,
            irq_token,
        })
    }

    pub fn ack(&self) -> Result<()> {
        irq_ack(self.irq_token)
    }
}
