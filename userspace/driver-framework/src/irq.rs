use libcluu::syscall::{irq_ack, irq_attach, ipc_recv};
use libcluu::Error;

pub struct IrqGuard {
    irq_token: usize,
    endpoint: usize,
    irq_number: usize,
}

impl IrqGuard {
    pub fn attach(irq_token: usize, endpoint: usize, irq: usize) -> Result<Self, Error> {
        irq_attach(irq_token, endpoint, irq)?;
        Ok(Self { irq_token, endpoint, irq_number: irq })
    }

    pub fn wait(&self, buf: &mut [u8]) -> Result<usize, Error> {
        ipc_recv(self.endpoint, buf)
    }

    pub fn ack(&self) -> Result<(), Error> {
        irq_ack(self.irq_token)
    }

    pub fn irq_number(&self) -> usize {
        self.irq_number
    }
}
