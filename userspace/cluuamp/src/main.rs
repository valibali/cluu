#![no_std]
#![no_main]

extern crate alloc;
extern crate nanomp3;

use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::boot::stdout;
use libcluu::fs::client::VfsClient;
use libcluu::posix::tty::{enter_raw, restore};
use libcluu::registry;
use libcluu::{debug_print, Result};

use libtui::diff::ScreenBuffer;
use libtui::input::{decode, StdinReader};
use libtui::render::{Renderer, CLEAR_SCREEN, RESET_SGR};

use cluu_cluuamp::{layout, model, view};

const TICK_MS: usize = 13;
const TIOCGWINSZ: u32 = 0x5413;

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

extern "C" {
    fn _ioctl(fd: i32, req: u32, arg: *mut core::ffi::c_void) -> i32;
}

use core::sync::atomic::{AtomicU16, Ordering};

static CACHED_W: AtomicU16 = AtomicU16::new(0);
static CACHED_H: AtomicU16 = AtomicU16::new(0);

fn terminal_size() -> (usize, usize) {
    let cw = CACHED_W.load(Ordering::Relaxed);
    let ch = CACHED_H.load(Ordering::Relaxed);
    if cw > 0 && ch > 0 {
        return (cw as usize, ch as usize);
    }
    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    for _ in 0..10 {
        let ret = unsafe { _ioctl(1, TIOCGWINSZ, &mut ws as *mut _ as *mut core::ffi::c_void) };
        if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            CACHED_W.store(ws.ws_col, Ordering::Relaxed);
            CACHED_H.store(ws.ws_row, Ordering::Relaxed);
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
        let _ = libcluu::syscall::yield_cpu();
    }
    (80, 25)
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("cluuamp: error {:?}\n", e));
            1
        }
    }
}

fn run() -> Result<()> {
    let args = libcluu::args::args();
    let playlist: Vec<String> = if args.len() >= 2 {
        args[1..].to_vec()
    } else {
        Vec::new()
    };

    debug_print("CLUUAMP_STARTING\n");

    let (w, h) = terminal_size();
    debug_print(&format!("cluuamp: terminal {}x{}\n", w, h));
    let mut mdl = model::CluuampModel::new(playlist, w, h);

    if mdl.audio.playlist().is_empty() {
        mdl.open_browser("/host");
    }

    let tty_ep = stdout();
    let saved_tty = enter_raw(tty_ep)?;
    let renderer = Renderer::new();
    renderer.enter_alt_screen();
    renderer.clear_screen();
    renderer.write(b"\x1b[?25l");

    let mut reader = StdinReader::new();
    let mut prev_buffer = ScreenBuffer::new(0, 0);

    let result = event_loop(&mut mdl, &renderer, &mut reader, &mut prev_buffer);

    renderer.write(RESET_SGR);
    renderer.write(b"\x1b[?25h");
    renderer.clear_screen();
    renderer.exit_alt_screen();
    restore(saved_tty)?;

    debug_print("CLUUAMP_DONE\n");
    result
}

fn event_loop(
    mdl: &mut model::CluuampModel,
    renderer: &Renderer,
    reader: &mut StdinReader,
    prev_buffer: &mut ScreenBuffer,
) -> Result<()> {
    loop {
        let (w, h) = terminal_size();
        if w != prev_buffer.width() || h != prev_buffer.height() {
            renderer.write(CLEAR_SCREEN);
            *prev_buffer = ScreenBuffer::new(0, 0);
            mdl.on_resize(w, h);
        }

        if mdl.browser_just_closed {
            mdl.browser_just_closed = false;
            renderer.write(b"\x1b[0m\x1b[2J\x1b[H");
            *prev_buffer = ScreenBuffer::new(0, 0);
        }

        if mdl.force_redraw {
            mdl.force_redraw = false;
            renderer.write(b"\x1b[0m\x1b[2J\x1b[H");
            *prev_buffer = ScreenBuffer::new(0, 0);
        }

        if mdl.confirm_just_happened {
            mdl.confirm_just_happened = false;
            let _ = mdl.audio.play();
        }

        if let Err(e) = mdl.tick() {
            debug_print(&format!("cluuamp: tick error {:?}\n", e));
        }

        let view = view::render(mdl);
        let mut new_buffer = ScreenBuffer::new(view.width, view.height);
        for (i, cell) in view.cells.iter().enumerate() {
            let row = if view.width > 0 { i / view.width } else { 0 };
            let col = if view.width > 0 { i % view.width } else { 0 };
            new_buffer.set(row, col, *cell);
        }

        let diff = new_buffer.diff_render(prev_buffer);
        if !diff.is_empty() {
            renderer.write(diff.as_bytes());
        }
        *prev_buffer = new_buffer;

        if mdl.should_quit {
            return Ok(());
        }

        if reader.wait_for_data(TICK_MS) {
            while reader.has_data() {
                if let Some(key) = decode(reader) {
                    mdl.handle_key(key);
                    if mdl.should_quit {
                        return Ok(());
                    }
                }
            }
        }

        if let Some(path) = mdl.take_pending_dir_list() {
            let entries = list_directory(&path);
            debug_print(&format!("cluuamp: listed {} entries for {}\n", entries.len(), path));
            mdl.browser_listed(entries);
        }
    }
}

fn list_directory(path: &str) -> Vec<libtui::components::browser::DirEntry> {
    let mut result: Vec<libtui::components::browser::DirEntry> = Vec::new();

    if path != "/" {
        result.push(libtui::components::browser::DirEntry {
            name: alloc::string::String::from(".."),
            kind: libtui::components::browser::EntryKind::Directory,
            size: 0,
        });
    }

    let ep = match registry::lookup_service("vfs:main") {
        Some(e) => e,
        None => return result,
    };
    let cid = registry::control_endpoint();
    let vfs = VfsClient::new(ep, cid);
    match vfs.readdir(path) {
        Ok(mut entries) => {
            entries.sort_by(|a, b| {
                let a_dir = a.is_dir;
                let b_dir = b.is_dir;
                b_dir.cmp(&a_dir).then(a.name.cmp(&b.name))
            });
            for e in entries {
                if e.name == "." || e.name == ".." {
                    continue;
                }
                let kind = if e.is_dir {
                    libtui::components::browser::EntryKind::Directory
                } else {
                    libtui::components::browser::EntryKind::File
                };
                if !e.name.ends_with(".mp3") && kind == libtui::components::browser::EntryKind::File {
                    continue;
                }
                result.push(libtui::components::browser::DirEntry {
                    name: e.name,
                    kind,
                    size: e.stat.size,
                });
            }
        }
        Err(_) => {}
    }
    result
}
