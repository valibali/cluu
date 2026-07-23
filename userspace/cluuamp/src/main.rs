#![no_std]
#![no_main]

extern crate alloc;

use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use libcluu::boot::{process_info, stdout, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::posix::tty::{enter_raw, restore};
use libcluu::registry;
use libcluu::syscall::space_map_range;
use libcluu::thread::{join, sleep_ms, spawn, Shared};
use libcluu::{debug_print, Result};

use libtui::diff::ScreenBuffer;
use libtui::input::{decode, StdinReader};
use libtui::render::{Renderer, CLEAR_SCREEN, RESET_SGR};

use cluu_cluuamp::{id3, layout, model, terminal, view};

const TICK_MS: u64 = 13;
const RENDER_MS: usize = 33;
const META_SCRATCH_VA: usize = 0x7100_0000;
const META_SCRATCH_PAGES: usize = 1;
const META_READ_LEN: usize = 4096;
const META_AUDIO_READ_LEN: usize = 4096;
const META_AUDIO_SCRATCH_PAGES: usize = 1;

struct AppState {
    model: model::CluuampModel,
    quit: AtomicU32,
}

fn audio_thread(shared: Shared<AppState>) {
    loop {
        let saturated;
        {
            let mut s = shared.lock();
            if s.quit.load(Ordering::Relaxed) != 0 {
                break;
            }
            if let Err(e) = s.model.audio_tick() {
                debug_print(&format!("cluuamp: audio tick error {:?}\n", e));
            }
            saturated = s.model.audio.ring_saturated();
        }

        let sleep = if saturated { 50 } else { TICK_MS };
        sleep_ms(sleep);
    }
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

    let (w, h) = terminal::current_size();
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

    let shared = Shared::new(AppState {
        model: mdl,
        quit: AtomicU32::new(0),
    });

    let tid = spawn({
        let shared = shared.clone();
        move || audio_thread(shared)
    });
    if tid != 0 {
        debug_print("cluuamp: audio thread started\n");
    } else {
        debug_print("cluuamp: pthread_create failed\n");
    }

    let result = ui_loop(&shared, &renderer, &mut reader, &mut prev_buffer);

    {
        let s = shared.lock();
        s.quit.store(1, Ordering::Relaxed);
    }
    sleep_ms(50);
    if tid != 0 {
        join(tid);
    }

    renderer.write(RESET_SGR);
    renderer.write(b"\x1b[?25h");
    renderer.clear_screen();
    renderer.exit_alt_screen();
    restore(saved_tty)?;

    debug_print("CLUUAMP_DONE\n");
    result
}

fn ui_loop(
    shared: &Shared<AppState>,
    renderer: &Renderer,
    reader: &mut StdinReader,
    prev_buffer: &mut ScreenBuffer,
) -> Result<()> {
    let mut view_buf = libtui::View::new(0, 0);
    let mut curr_buffer = ScreenBuffer::new(0, 0);
    let mut meta_buf: Vec<u8> = Vec::with_capacity(META_READ_LEN);
    let mut diff_buf: Vec<u8> = Vec::with_capacity(8192);
    let mut frame_counter: u64 = 0;
    loop {
        let (w, h) = terminal::current_size();

        let (view, should_quit, pending_dir, pending_dir_import, pending_meta) = {
            let mut s = shared.lock();

            if w != prev_buffer.width() || h != prev_buffer.height() {
                renderer.write(CLEAR_SCREEN);
                *prev_buffer = ScreenBuffer::new(0, 0);
                s.model.on_resize(w, h);
            }

            let browser_just_closed = s.model.browser_just_closed;
            if browser_just_closed {
                s.model.browser_just_closed = false;
                renderer.write(b"\x1b[0m\x1b[2J\x1b[H");
                *prev_buffer = ScreenBuffer::new(0, 0);
            }

            let force_redraw = s.model.force_redraw;
            if force_redraw {
                s.model.force_redraw = false;
                renderer.write(b"\x1b[0m\x1b[2J\x1b[H");
                *prev_buffer = ScreenBuffer::new(0, 0);
            }

            if s.model.confirm_just_happened {
                s.model.confirm_just_happened = false;
                let _ = s.model.audio.play();
            }

            let _ = s.model.audio.service_pending();

            s.model.ui_tick();

            let was_playing = s.model.audio.state() == cluu_cluuamp::audio::PlaybackState::Playing;
            let structural_change = force_redraw || browser_just_closed;
            let needs_render = structural_change
                || s.model.title_scroll_changed()
                || was_playing
                || s.model.browser.is_some()
                || s.model.dirty;
            s.model.dirty = false;

            let view = if needs_render {
                view::render_into(&mut view_buf, &mut s.model);
                s.model.mark_title_rendered();
                Some(())
            } else {
                None
            };

            let should_quit = s.model.should_quit;
            let pending_dir = s.model.take_pending_dir_list();
            let pending_dir_import = s.model.take_pending_dir_import();
            let pending_meta = if !should_quit {
                s.model.audio.next_unparsed_meta()
            } else {
                None
            };

            (view, should_quit, pending_dir, pending_dir_import, pending_meta)
        };

        if view.is_some() {
            curr_buffer.resize(view_buf.width, view_buf.height);
            for (i, cell) in view_buf.cells.iter().enumerate() {
                let row = if view_buf.width > 0 { i / view_buf.width } else { 0 };
                let col = if view_buf.width > 0 { i % view_buf.width } else { 0 };
                curr_buffer.set(row, col, *cell);
            }
            curr_buffer.diff_render_into(prev_buffer, &mut diff_buf);
            if !diff_buf.is_empty() {
                renderer.write(&diff_buf);
            }
            core::mem::swap(prev_buffer, &mut curr_buffer);
        }

        if should_quit {
            return Ok(());
        }

        frame_counter += 1;
        if frame_counter % 300 == 0 {
            let st = libcluu::allocator::stats();
            debug_print(&format!(
                "cluuamp: heap total={} used={} peak={} free={} largest_free={} leaked={}\n",
                st.total, st.used, st.peak, st.free, st.largest_free, st.leaked_deallocs
            ));
        }

        if reader.wait_for_data(RENDER_MS) {
            while reader.has_data() {
                if let Some(key) = decode(reader) {
                    let quit = {
                        let mut s = shared.lock();
                        s.model.handle_key(key);
                        s.model.should_quit
                    };
                    if quit {
                        return Ok(());
                    }
                }
            }
        }

        if let Some(path) = pending_dir {
            let entries = list_directory(&path);
            debug_print(&format!("cluuamp: listed {} entries for {}\n", entries.len(), path));
            let mut s = shared.lock();
            s.model.browser_listed(entries);
        }

        if let Some(path) = pending_dir_import {
            let entries = list_directory(&path);
            let mut mp3_paths: Vec<String> = entries.into_iter()
                .filter(|e| e.kind == libtui::components::browser::EntryKind::File && e.name.ends_with(".mp3"))
                .map(|e| {
                    if path == "/" {
                        alloc::format!("/{}", e.name)
                    } else {
                        alloc::format!("{}/{}", path, e.name)
                    }
                })
                .collect();
            mp3_paths.sort();
            let mut s = shared.lock();
            s.model.audio.extend_playlist(mp3_paths);
            s.model.dirty = true;
        }

        if let Some(idx) = pending_meta {
            let path = {
                let s = shared.lock();
                s.model.audio.playlist().get(idx).cloned()
            };
            if let Some(path) = path {
                if let Some(file_size) = read_file_head_into(&path, META_READ_LEN, &mut meta_buf) {
                    let mut meta = id3::parse(&meta_buf);
                    let audio_offset = id3::id3v2_tag_size(&meta_buf);
                    if audio_offset > 0 && audio_offset < file_size {
                        if read_audio_head_into(&path, audio_offset, META_AUDIO_READ_LEN, &mut meta_buf).is_some() {
                            meta.duration_ms = cluu_cluuamp::audio::AudioEngine::estimate_duration_ms(&meta_buf, file_size.saturating_sub(audio_offset));
                        }
                    } else if audio_offset == 0 {
                        meta.duration_ms = cluu_cluuamp::audio::AudioEngine::estimate_duration_ms(&meta_buf, file_size);
                    }
                    let mut s = shared.lock();
                    s.model.audio.set_track_meta(idx, meta);
                    s.model.dirty = true;
                } else {
                    let mut s = shared.lock();
                    s.model.audio.set_track_meta(idx, id3::TrackMeta::default());
                }
            }
        }
    }
}

fn read_file_head_into(path: &str, len: usize, buf: &mut Vec<u8>) -> Option<usize> {
    buf.clear();
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let _ = space_map_range(space_token, META_SCRATCH_VA, 0, 0x03, META_SCRATCH_PAGES, 0);

    let ep = registry::lookup_service("vfs:main")?;
    let cid = registry::control_endpoint();
    let vfs = VfsClient::new(ep, cid);
    let file = vfs.open(path).ok()?;
    let read_len = len.min(file.size);
    let grant = vfs.read_grant(file, 0, read_len, space_token, META_SCRATCH_VA).ok()?;
    let bytes = unsafe { core::slice::from_raw_parts(grant.base as *const u8, grant.len) };
    buf.extend_from_slice(bytes);
    let file_size = file.size;
    let _ = vfs.close(file);
    Some(file_size)
}

fn read_audio_head_into(path: &str, offset: usize, len: usize, buf: &mut Vec<u8>) -> Option<usize> {
    buf.clear();
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let _ = space_map_range(space_token, META_SCRATCH_VA, 0, 0x03, META_AUDIO_SCRATCH_PAGES, 0);

    let ep = registry::lookup_service("vfs:main")?;
    let cid = registry::control_endpoint();
    let vfs = VfsClient::new(ep, cid);
    let file = vfs.open(path).ok()?;
    let read_len = len.min(file.size.saturating_sub(offset));
    let grant = vfs.read_grant(file, offset, read_len, space_token, META_SCRATCH_VA).ok()?;
    let bytes = unsafe { core::slice::from_raw_parts(grant.base as *const u8, grant.len) };
    buf.extend_from_slice(bytes);
    let file_size = file.size;
    let _ = vfs.close(file);
    Some(file_size)
}

fn list_directory(path: &str) -> Vec<libtui::components::browser::DirEntry> {
    let mut result: Vec<libtui::components::browser::DirEntry> = Vec::new();

    if path != "/" {
        result.push(libtui::components::browser::DirEntry {
            name: alloc::string::String::from("./"),
            kind: libtui::components::browser::EntryKind::Directory,
            size: 0,
        });
        result.push(libtui::components::browser::DirEntry {
            name: alloc::string::String::from("../"),
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
