#![no_std]
#![no_main]

// FFI bindings to C syscall library
mod syscalls {
    pub const STDOUT: i32 = 1;

    // Port ID type
    pub type PortId = isize;

    // IPC Message (256 bytes)
    pub const IPC_MSG_SIZE: usize = 256;

    #[repr(C)]
    pub struct IpcMessage {
        pub data: [u8; IPC_MSG_SIZE],
    }

    unsafe extern "C" {
        pub fn syscall_write(fd: i32, buf: *const u8, count: usize) -> isize;
        pub fn syscall_exit(code: i32) -> !;

        // IPC syscalls
        pub fn port_create() -> PortId;
        pub fn port_recv(port: PortId, msg: *mut IpcMessage) -> i32;
        pub fn port_send(port: PortId, msg: *const IpcMessage) -> i32;
        pub fn register_port_name(name: *const u8, port: PortId) -> i32;
    }
}

// Helper functions for I/O
fn print(s: &str) {
    unsafe {
        syscalls::syscall_write(syscalls::STDOUT, s.as_ptr(), s.len());
    }
}

fn print_dec(mut n: i32) {
    if n < 0 {
        print("-");
        n = -n;
    }

    if n == 0 {
        print("0");
        return;
    }

    let mut buf = [0u8; 12];
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }

    while i > 0 {
        i -= 1;
        unsafe {
            syscalls::syscall_write(syscalls::STDOUT, &buf[i] as *const u8, 1);
        }
    }
}

// Shmem server message types
#[repr(C)]
#[derive(Clone, Copy)]
struct ShmemRequest {
    op: u32,           // Operation: CREATE=1, ATTACH=2, DETACH=3, DESTROY=4
    region_id: usize,  // Shared memory region ID
    size: usize,       // Size for CREATE operation
    flags: u32,        // Permissions flags
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ShmemResponse {
    status: i32,       // 0=success, negative=error
    region_id: usize,  // Region ID for CREATE
    addr: usize,       // Virtual address for ATTACH
}

// Shmem server state
struct ShmemServer {
    port_id: syscalls::PortId,
    next_region_id: usize,
}

impl ShmemServer {
    fn new() -> Option<Self> {
        let port_id = unsafe { syscalls::port_create() };
        if port_id < 0 {
            print("[shmem_server] ERROR: Failed to create IPC port\n");
            return None;
        }

        print("[shmem_server] Created IPC port\n");
        print("[shmem_server] Port ID: ");
        print_dec(port_id as i32);
        print("\n");

        // Register well-known port name
        let port_name = b"shmem_server\0";
        let result = unsafe {
            syscalls::register_port_name(port_name.as_ptr(), port_id)
        };

        if result < 0 {
            print("[shmem_server] WARNING: Failed to register port name\n");
        } else {
            print("[shmem_server] Registered as 'shmem_server'\n");
        }

        Some(Self {
            port_id,
            next_region_id: 1,
        })
    }

    fn handle_request(&mut self, req: &ShmemRequest) -> ShmemResponse {
        match req.op {
            1 => self.handle_create(req),
            2 => self.handle_attach(req),
            3 => self.handle_detach(req),
            4 => self.handle_destroy(req),
            _ => ShmemResponse {
                status: -1, // EINVAL
                region_id: 0,
                addr: 0,
            },
        }
    }

    fn handle_create(&mut self, req: &ShmemRequest) -> ShmemResponse {
        // For now, just allocate a region ID
        // TODO: Actually allocate physical memory via kernel syscall
        let region_id = self.next_region_id;
        self.next_region_id += 1;

        print("[shmem_server] CREATE: size=");
        print_dec(req.size as i32);
        print(" -> region_id=");
        print_dec(region_id as i32);
        print("\n");

        ShmemResponse {
            status: 0,
            region_id,
            addr: 0,
        }
    }

    fn handle_attach(&mut self, req: &ShmemRequest) -> ShmemResponse {
        // TODO: Map the region into caller's address space
        print("[shmem_server] ATTACH: region_id=");
        print_dec(req.region_id as i32);
        print("\n");

        ShmemResponse {
            status: 0,
            region_id: req.region_id,
            addr: 0x10000000, // Placeholder address
        }
    }

    fn handle_detach(&mut self, req: &ShmemRequest) -> ShmemResponse {
        // TODO: Unmap the region from caller's address space
        print("[shmem_server] DETACH: region_id=");
        print_dec(req.region_id as i32);
        print("\n");

        ShmemResponse {
            status: 0,
            region_id: req.region_id,
            addr: 0,
        }
    }

    fn handle_destroy(&mut self, req: &ShmemRequest) -> ShmemResponse {
        // TODO: Free the physical memory
        print("[shmem_server] DESTROY: region_id=");
        print_dec(req.region_id as i32);
        print("\n");

        ShmemResponse {
            status: 0,
            region_id: req.region_id,
            addr: 0,
        }
    }

    fn run(&mut self) -> ! {
        print("[shmem_server] Server loop started\n");

        let mut msg_buf = syscalls::IpcMessage {
            data: [0u8; syscalls::IPC_MSG_SIZE],
        };

        loop {
            // Receive IPC message (blocking)
            let result = unsafe {
                syscalls::port_recv(self.port_id, &mut msg_buf as *mut syscalls::IpcMessage)
            };

            if result < 0 {
                print("[shmem_server] ERROR: port_recv failed with code ");
                print_dec(result);
                print("\n");
                continue;
            }

            // Parse request
            let req = unsafe {
                &*(msg_buf.data.as_ptr() as *const ShmemRequest)
            };

            // Handle request
            let resp = self.handle_request(req);

            // Send response
            // TODO: Need sender's port ID to send response back
            // For now, we can't reply - need to extend IPC protocol
            // to include reply port in the message
            let _ = resp; // Suppress unused warning
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print("[shmem_server] Starting shared memory server...\n");

    // Create server
    let mut server = match ShmemServer::new() {
        Some(s) => s,
        None => {
            print("[shmem_server] ERROR: Failed to create server\n");
            unsafe { syscalls::syscall_exit(1) }
        }
    };

    print("[shmem_server] Server initialized and ready\n");

    // Run server loop
    server.run();
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    print("[shmem_server] PANIC!\n");
    unsafe { syscalls::syscall_exit(1) }
}
