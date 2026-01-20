use bitflags::bitflags;

bitflags! {
    #[derive(Copy, Clone)]
    /// Explicit rights bitmask (matches kernel definitions)
    pub struct Rights: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
        const CREATE = 1 << 3;
        const DESTROY = 1 << 4;
        const GRANT = 1 << 5;
        const MAP = 1 << 6;
        const MANAGE = 1 << 7;

        const THREAD_CONTROL = 1 << 8;
        const THREAD_SUSPEND = 1 << 9;

        const SPACE_MAP = 1 << 16;
        const SPACE_UNMAP = 1 << 17;
        const SPACE_GRANT = 1 << 18;

        const IPC_SEND = 1 << 24;
        const IPC_RECV = 1 << 25;
        const IPC_CALL = 1 << 26;

        const IRQ_HANDLE = 1 << 28;
        const IRQ_ACK = 1 << 29;

        const PCI_ACCESS = 1 << 30;
    }
}

impl Rights {
    pub fn thread_full() -> Self {
        Self::READ | Self::WRITE | Self::THREAD_CONTROL | Self::THREAD_SUSPEND | Self::DESTROY
    }

    pub fn space_full() -> Self {
        Self::READ | Self::SPACE_MAP | Self::SPACE_UNMAP | Self::SPACE_GRANT | Self::DESTROY
    }

    pub fn ipc_full() -> Self {
        Self::IPC_SEND | Self::IPC_RECV | Self::IPC_CALL
    }

    pub fn irq_full() -> Self {
        Self::IRQ_HANDLE | Self::IRQ_ACK
    }
}
