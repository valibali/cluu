#![no_std]
#![no_main]

extern crate alloc;

use alloc::{collections::BTreeMap, format};
use libcluu::boot::{procmgr_info, ParentInfo, ProcInfo, PARENT_INFO_ADDR, PROC_INFO_ADDR};
use libcluu::elf::{ElfFile, LoadableSegment};
use libcluu::syscall::thread_destroy;
use libcluu::tar::find_member;
use libcluu::*;

const SERVICE_STACK_SIZE: usize = 64 * 1024;
const SERVICE_STACK_BASE: usize = 0x6d000000;
const SERVICE_STACK_TOP: usize = SERVICE_STACK_BASE + SERVICE_STACK_SIZE;
const PAGE_SIZE: usize = 4096;
const STACK_FLAGS: usize = 0x03; // read + write
const SERVICE_PATH: &str = "bin/shell";
const PROCMGR_EXIT_LABEL: u32 = 1;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = main_result() {
        let _ = debug_print(&format!("procmgr: fatal error {:?}", err));
        loop {
            let _ = yield_cpu();
        }
    }
    0
}

fn main_result() -> Result<()> {
    let mut manager = ProcessManager::new()?;
    manager.init()?;
    manager.run()
}

struct ProcessManager {
    token: usize,
    exit_endpoint: usize,
    tty_endpoint: usize,
    initrd_size: usize,
    exit_cookie_next: usize,
    exit_table: BTreeMap<usize, usize>,
}

impl ProcessManager {
    fn new() -> Result<Self> {
        let info = procmgr_info();
        Ok(Self {
            token: info.token,
            exit_endpoint: info.exit_endpoint,
            tty_endpoint: info.tty_endpoint,
            initrd_size: info.initrd_size as usize,
            exit_cookie_next: 1,
            exit_table: BTreeMap::new(),
        })
    }

    fn init(&mut self) -> Result<()> {
        debug_print("=========================================")?;
        debug_print("  Process Manager Starting")?;
        debug_print("=========================================")?;
        debug_print("Derived procmgr token handle")?;
        debug_print(&format!("  Handle: {}", self.token))?;

        self.spawn_service(SERVICE_PATH, 200)?;
        debug_print("Service spawned; yielding to scheduler")?;
        yield_cpu()?;
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        loop {
            self.poll_exit_notifications()?;
        }
    }

    fn poll_exit_notifications(&mut self) -> Result<()> {
        let mut msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 0);
        if let Err(err) = recv(self.exit_endpoint, &mut msg, IpcFlags::empty()) {
            debug_print(&format!("TRACE: exit recv failed {:?}", err))?;
            return Ok(());
        }

        if msg.tag.label != PROCMGR_EXIT_LABEL || msg.tag.words < 2 {
            debug_print(&format!(
                "TRACE: exit msg label {} words {}",
                msg.tag.label, msg.tag.words
            ))?;
            return Ok(());
        }

        let cookie = msg.words[0];
        let exit_code = msg.words[1] as i32;
        let thread_token = match self.exit_table.remove(&cookie) {
            Some(token) => token,
            None => return Ok(()),
        };

        debug_print(&format!(
            "procmgr: exit cookie {} (code {})",
            cookie, exit_code
        ))?;
        if thread_destroy(thread_token).is_ok() {
            debug_print(&format!("TRACE: reaped thread token {}", thread_token))?;
        }
        Ok(())
    }

    fn next_exit_cookie(&mut self) -> usize {
        let cookie = self.exit_cookie_next;
        self.exit_cookie_next = self.exit_cookie_next.wrapping_add(1);
        cookie
    }

    fn spawn_service(&mut self, path: &str, priority: usize) -> Result<()> {
        let initrd =
            unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, self.initrd_size) };
        let service_bytes = find_member(initrd, path).ok_or(Error::NotFound)?;

        let elf = ElfFile::parse(service_bytes)?;
        debug_print("Parsed service ELF")?;

        let space_token = space_create(self.token)?;
        map_segments(space_token, &elf, service_bytes)?;
        map_stack(space_token)?;

        let send_rights = Rights::IPC_SEND.bits() as usize;
        let child_endpoint = token_derive(self.exit_endpoint, send_rights, u64::MAX)?;
        let cookie = self.next_exit_cookie();
        debug_print(&format!(
            "TRACE: child exit ep {} cookie {}",
            child_endpoint, cookie
        ))?;
        let stdin_endpoint = endpoint_create(self.token)?;
        let tty_send = token_derive(self.tty_endpoint, send_rights, u64::MAX)?;
        map_process_info_page(
            space_token,
            child_endpoint,
            cookie,
            stdin_endpoint,
            tty_send,
            tty_send,
            tty_send,
        )?;
        register_tty(stdin_endpoint, self.tty_endpoint)?;

        let thread_token = thread_create(
            space_token,
            elf.entry_point as usize,
            SERVICE_STACK_TOP,
            priority,
        )?;

        self.exit_table.insert(cookie, thread_token);
        Ok(())
    }
}

fn map_segments(space_token: usize, elf: &ElfFile, bytes: &[u8]) -> Result<()> {
    for segment in elf.segments_iter() {
        map_segment(space_token, segment, bytes)?;
    }
    Ok(())
}

fn map_segment(space_token: usize, segment: &LoadableSegment, bytes: &[u8]) -> Result<()> {
    let start = segment.vaddr as usize;
    if start % PAGE_SIZE != 0 {
        return Err(Error::InvalidArgument);
    }

    let mem_size = segment.mem_size as usize;
    if mem_size == 0 {
        return Ok(());
    }

    let file_offset = segment.file_offset as usize;
    let file_size = segment.file_size as usize;
    if file_offset + file_size > bytes.len() {
        return Err(Error::InvalidArgument);
    }

    let slice = &bytes[file_offset..file_offset + file_size];
    let mut mapped = 0usize;
    while mapped < mem_size {
        let virt = start + mapped;
        let remaining = file_size.saturating_sub(mapped);
        let copy_len = remaining.min(PAGE_SIZE);
        let ptr = if copy_len > 0 {
            slice[mapped..mapped + copy_len].as_ptr() as usize
        } else {
            0
        };

        space_map(
            space_token,
            virt,
            ptr,
            segment.page_flags() as usize,
            copy_len,
        )?;

        mapped += PAGE_SIZE;
    }

    Ok(())
}

fn map_stack(space_token: usize) -> Result<()> {
    let mut addr = SERVICE_STACK_TOP - SERVICE_STACK_SIZE;
    while addr < SERVICE_STACK_TOP {
        space_map(space_token, addr, 0, STACK_FLAGS, 0)?;
        addr += PAGE_SIZE;
    }
    Ok(())
}

fn map_process_info_page(
    space_token: usize,
    exit_endpoint: usize,
    exit_cookie: usize,
    stdin_endpoint: usize,
    stdout_endpoint: usize,
    stderr_endpoint: usize,
    stdlog_endpoint: usize,
) -> Result<()> {
    const PAGE_SIZE: usize = 4096;
    const READ_ONLY: usize = 0x01;
    let page_base = PARENT_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut page = [0u8; PAGE_SIZE];
    let parent_info = ParentInfo {
        exit_endpoint,
        exit_cookie,
    };
    let parent_bytes = unsafe {
        core::slice::from_raw_parts(
            &parent_info as *const ParentInfo as *const u8,
            core::mem::size_of::<ParentInfo>(),
        )
    };
    let parent_offset = PARENT_INFO_ADDR - page_base;
    let parent_end = parent_offset + parent_bytes.len();
    if parent_end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[parent_offset..parent_end].copy_from_slice(parent_bytes);

    let info = ProcInfo {
        stdin_endpoint,
        stdout_endpoint,
        stderr_endpoint,
        stdlog_endpoint,
    };
    let proc_bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const ProcInfo as *const u8,
            core::mem::size_of::<ProcInfo>(),
        )
    };
    let proc_offset = PROC_INFO_ADDR - page_base;
    let proc_end = proc_offset + proc_bytes.len();
    if proc_end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[proc_offset..proc_end].copy_from_slice(proc_bytes);

    space_map(
        space_token,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        PAGE_SIZE,
    )?;
    Ok(())
}

fn register_tty(stdin_endpoint: usize, tty_endpoint: usize) -> Result<()> {
    let msg = Message::new(
        libcluu::ipc::TTY_REGISTER_LABEL,
        [stdin_endpoint, 0, 0, 0, 0, 0],
        1,
    );
    libcluu::ipc::send(tty_endpoint, &msg, IpcFlags::empty())?;
    Ok(())
}
