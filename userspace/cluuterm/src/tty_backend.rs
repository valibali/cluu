//! Cluuterm core state machine and recv loop. Task 15 fills in handlers.

use libcluu::window_shm::WindowShm;

pub struct Cluuterm {
    pub cols: usize,
    pub rows: usize,
    /// Pointer to the SHM header mapped at SHM_VA.
    pub shm: *mut WindowShm,
    pub pts_id: u32,
    pub window_id: u32,
    /// My endpoint (receives FRAME_READY + INPUT_FORWARD from compositor,
    /// PTS_READ/WRITE from VFS, PTS_CLOSED from VFS).
    pub my_ep: usize,
    /// Compositor client endpoint (for DAMAGE + DESTROY messages).
    pub comp_ep: usize,
}

impl Cluuterm {
    pub fn new(
        cols: usize,
        rows: usize,
        shm: *mut WindowShm,
        pts_id: u32,
        window_id: u32,
        my_ep: usize,
        comp_ep: usize,
    ) -> Self {
        Self {
            cols,
            rows,
            shm,
            pts_id,
            window_id,
            my_ep,
            comp_ep,
        }
    }

    /// Main event loop.
    ///
    /// Task 15 replaces this with the real recv loop that handles
    /// FRAME_READY, INPUT_FORWARD, PTS_READ, PTS_WRITE, and PTS_CLOSED.
    pub fn run(&mut self) {
        // Stub: block forever waiting on our endpoint so we don't spin-waste
        // CPU. We time out every 10 s and go back to sleep — a real loop
        // will be inserted by Task 15.
        let mut buf = [0u8; 256];
        let tokens = [self.my_ep];
        loop {
            match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, 10_000) {
                Ok(_) => {
                    // TODO(task15): dispatch on label.
                }
                Err(_) => {
                    // Timeout or spurious error — keep waiting.
                }
            }
        }
    }
}
