#![no_std]
#![no_main]

extern crate alloc;
extern crate zune_jpeg;

use alloc::format;
use alloc::vec::Vec;

use libcluu::runtime as _;

use libcluu::boot::{process_info, space_token, TOKEN_IPC, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    self, COMP_FRAME_READY_LABEL, COMP_INPUT_FORWARD_LABEL,
    COMP_WIN_DESTROY_LABEL,
    COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY,
    COMP_WIN_FLAG_NO_CHROME,
};
use libcluu::IpcFlags;
use libcluu::registry;
use libcluu::syscall::{self, InvokeOp, MAP_FRAME_TOKEN};
use libcluu::types::Message;

use cluu_wire::display::{
    DISPLAY_OUTPUT_INFO_LABEL, DISPLAY_SURFACE_CREATE_LABEL,
    DISPLAY_SET_GEOMETRY_LABEL, DISPLAY_BUFFER_COMMIT_LABEL,
    DISPLAY_SURFACE_DESTROY_LABEL,
};

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const SHM_VA: usize = 0xD000_0000;
const DISPLAYD_VA: usize = 0xD200_0000;
const FILE_VA: usize = 0xE000_0000;
const FLAGS_USER_RW: usize = 0x07;
const PAGE_SIZE: usize = 4096;

/// Surface dimensions in pixels. Capped to screen at runtime.
const SURF_W: u32 = 640;
const SURF_H: u32 = 400;

const DEFAULT_JPEG_PATH: &str = "/host/00-netBlocVol24_320Kbs_MP3/2009 - Various Artists - netBloc Vol 24_ tiuqottigeloot/00 - Cover.jpg";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = libcluu::debug_print(&format!("imgview: error {:?}", e));
            -1
        }
    }
}

fn run() -> libcluu::Result<()> {
    let args = libcluu::args::args();
    let path: &str = if args.len() >= 2 {
        &args[1]
    } else {
        DEFAULT_JPEG_PATH
    };

    let abs_path = libcluu::posix::resolve_path(path);
    let _ = libcluu::debug_print(&format!("imgview: opening {}", abs_path));

    let jpeg_bytes = read_file(&abs_path)?;
    let _ = libcluu::debug_print(&format!("imgview: {} bytes", jpeg_bytes.len()));

    let (img_w, img_h, rgb) = decode_jpeg(&jpeg_bytes)?;
    let _ = libcluu::debug_print(&format!("imgview: decoded {}x{}", img_w, img_h));

    // --- displayd surface creation (T9: direct displayd, no compositor PixelRegion) ---
    let displayd_ep = registry::lookup_service("displayd:main")
        .ok_or(libcluu::Error::NotFound)?;
    let _ = libcluu::debug_print("imgview: displayd ep resolved");

    let mut info_msg = Message::new(DISPLAY_OUTPUT_INFO_LABEL, [0; 6], 0);
    ipc::call(displayd_ep, &mut info_msg, IpcFlags::empty())?;
    let screen_w = info_msg.words[0] as u32;
    let screen_h = info_msg.words[1] as u32;
    let _ = libcluu::debug_print(&format!("imgview: screen {}x{}", screen_w, screen_h));

    let surf_w = SURF_W.min(screen_w);
    let surf_h = SURF_H.min(screen_h);
    let pitch = surf_w * 4;

    let mut create_msg = Message::new(
        DISPLAY_SURFACE_CREATE_LABEL,
        [0, surf_w as usize, surf_h as usize, pitch as usize, 0, 0],
        4,
    );
    ipc::call(displayd_ep, &mut create_msg, IpcFlags::empty())?;
    let surface_token = create_msg.words[0] as u64;
    if surface_token == 0 {
        return Err(libcluu::Error::InvalidArgument);
    }
    let _ = libcluu::debug_print(&format!("imgview: displayd surface {}x{} token={}", surf_w, surf_h, surface_token));

    // Center on screen, z=1 (above compositor), visible.
    let geo_x = ((screen_w - surf_w) / 2) as i32;
    let geo_y = ((screen_h - surf_h) / 2) as i32;
    let geo_msg = Message::new(
        DISPLAY_SET_GEOMETRY_LABEL,
        [0, surface_token as usize, geo_x as usize, geo_y as usize, 0, 0],
        4,
    );
    let mut geo_payload = [0u8; 5];
    geo_payload[0..4].copy_from_slice(&1i32.to_le_bytes());
    geo_payload[4] = 1;
    let _ = ipc::send_msg_with_payload(displayd_ep, &geo_msg, &geo_payload);

    // --- compositor window for input only (no pixel region) ---
    let (win_id, comp_ep, my_ep) = register_window()?;
    let _ = libcluu::debug_print(&format!("imgview: compositor window {} for input", win_id));

    // --- allocate frame token for pixel transfer to displayd ---
    let sp = space_token();
    let frame_bytes = (surf_w as usize) * (surf_h as usize) * 4;
    let frame_pages = (frame_bytes + PAGE_SIZE - 1) / PAGE_SIZE;
    let frame_token = unsafe {
        syscall::invoke(sp, InvokeOp::FrameAllocate, frame_bytes, 0, 0, 0)?
    } as u64;
    syscall::space_map_range(
        sp, DISPLAYD_VA, frame_token as usize,
        FLAGS_USER_RW | MAP_FRAME_TOKEN, frame_pages, 0,
    )?;

    // Zero the frame buffer.
    unsafe {
        core::ptr::write_bytes(DISPLAYD_VA as *mut u8, 0, frame_bytes);
    }

    // Scale image into the frame token.
    let frame_ptr = DISPLAYD_VA as *mut u32;
    scale_nearest(&rgb, img_w, img_h, frame_ptr, surf_w as usize, surf_h as usize);
    let _ = libcluu::debug_print(&format!("imgview: scaled {}x{} -> {}x{}", img_w, img_h, surf_w, surf_h));

    // Commit to displayd with damage covering the full surface.
    let mut damage_bytes = [0u8; 16];
    damage_bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
    damage_bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
    damage_bytes[8..12].copy_from_slice(&surf_w.to_le_bytes());
    damage_bytes[12..16].copy_from_slice(&surf_h.to_le_bytes());
    let commit_msg = Message::new(
        DISPLAY_BUFFER_COMMIT_LABEL,
        [0, surface_token as usize, 0, 0, frame_token as usize, 0],
        5,
    );
    let _ = ipc::send_msg_with_payload(displayd_ep, &commit_msg, &damage_bytes);
    let _ = libcluu::debug_print("imgview: committed to displayd, waiting for Esc");

    // --- input loop: wait for Esc via compositor ---
    let mut recv_buf = [0u8; 256];
    let tokens = [my_ep];
    loop {
        match syscall::ipc_recv_any(&tokens, &mut recv_buf, 60_000) {
            Ok((_idx, len)) => {
                if let Some((msg, _)) = ipc::parse_message(&recv_buf[..len]) {
                    if msg.tag.label == COMP_INPUT_FORWARD_LABEL {
                        let kind = msg.words[5] as u32;
                        let scancode = msg.words[3] as u8;
                        if kind == 99 || scancode == 0x01 {
                            let _ = libcluu::debug_print("imgview: exiting");
                            cleanup(
                                displayd_ep, surface_token,
                                sp, DISPLAYD_VA, frame_pages, frame_token,
                                comp_ep, win_id,
                            );
                            return Ok(());
                        }
                    }
                }
            }
            Err(_) => {
                let _ = syscall::yield_cpu();
            }
        }
    }
}

fn cleanup(
    displayd_ep: usize,
    surface_token: u64,
    sp: usize,
    frame_va: usize,
    frame_pages: usize,
    frame_token: u64,
    comp_ep: usize,
    win_id: u64,
) {
    let destroy_msg = Message::new(
        DISPLAY_SURFACE_DESTROY_LABEL,
        [0, surface_token as usize, 0, 0, 0, 0],
        2,
    );
    let _ = ipc::send(displayd_ep, &destroy_msg, IpcFlags::empty());

    let _ = syscall::space_unmap(sp, frame_va, frame_pages);
    if frame_token != 0 {
        unsafe {
            let _ = syscall::invoke(frame_token as usize, InvokeOp::FrameFree, 0, 0, 0, 0);
        }
    }

    let win_destroy = Message::new(
        COMP_WIN_DESTROY_LABEL,
        [win_id as usize, 0, 0, 0, 0, 0],
        1,
    );
    let _ = ipc::send(comp_ep, &win_destroy, IpcFlags::empty());
}

fn read_file(path: &str) -> libcluu::Result<Vec<u8>> {
    let vfs_ep = registry::subscribe_output("vfs", "main")?;
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_ep, client_id);

    let file = vfs.open(path)?;
    let file_size = file.size;

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    let num_pages = (file_size + 0xFFF) / 0x1000;
    syscall::space_map_range(space_token, FILE_VA, 0, 0x03, num_pages, 0)?;

    let grant = vfs.read_file_bulk(file, space_token, FILE_VA)?;

    let bytes = unsafe {
        core::slice::from_raw_parts(
            (FILE_VA + grant.offset) as *const u8,
            grant.len,
        )
    };
    let data = bytes.to_vec();

    let _ = syscall::space_unmap(space_token, FILE_VA, num_pages);
    vfs.close(file)?;

    Ok(data)
}

fn decode_jpeg(jpeg: &[u8]) -> libcluu::Result<(usize, usize, Vec<u8>)> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);

    decoder.decode_headers().map_err(|_| libcluu::Error::InvalidArgument)?;
    let info = decoder.info().ok_or(libcluu::Error::InvalidArgument)?;
    let w = info.width as usize;
    let h = info.height as usize;

    let mut pixels = alloc::vec![0u8; w * h * 3];
    decoder.decode_into(&mut pixels).map_err(|_| libcluu::Error::InvalidArgument)?;

    Ok((w, h, pixels))
}

/// Register a compositor window for input forwarding only.
/// No pixel region is set — display is handled by displayd.
fn register_window() -> libcluu::Result<(u64, usize, usize)> {
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = syscall::endpoint_create(ipc_cap)?;

    if registry::init("imgview").is_err() {
        return Err(libcluu::Error::InvalidState);
    }

    let comp_ep = registry::lookup_service("compositor:client")
        .ok_or(libcluu::Error::NotFound)?;

    let req_w: u16 = 80;
    let req_h: u16 = 40;

    let title = b"imgview";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [
            title.len(),
            req_w as usize,
            req_h as usize,
            my_ep,
            COMP_WIN_FLAG_NO_CHROME as usize,
            0,
        ],
        4,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    ipc::call_with_payload(comp_ep, &req, title, &mut reply)?;
    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        return Err(libcluu::Error::InvalidArgument);
    }
    let win_id = reply.words[0] as u64;
    let shm_token = reply.words[1];
    let gw = reply.words[2] as u16;
    let gh = reply.words[3] as u16;
    let err = reply.words[4];
    if err != 0 {
        return Err(libcluu::Error::PermissionDenied);
    }

    let cells_bytes = gw as usize * gh as usize * 8;
    let total = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let num_pages = total / 0x1000;
    let space = space_token();
    syscall::space_map_range(
        space, SHM_VA, shm_token as usize, FLAGS_USER_RW | MAP_FRAME_TOKEN, num_pages, 0,
    )?;

    let fr_msg = Message::new(COMP_FRAME_READY_LABEL, [win_id as usize, 0, 0, 0, 0, 0], 1);
    let _ = ipc::send(my_ep, &fr_msg, IpcFlags::empty());

    Ok((win_id, comp_ep, my_ep))
}

/// Nearest-neighbor scale from src RGB to dst ARGB32 frame token.
fn scale_nearest(
    src_rgb: &[u8],
    src_w: usize,
    src_h: usize,
    dst: *mut u32,
    dst_w: usize,
    dst_h: usize,
) {
    for dy in 0..dst_h {
        let sy = ((dy as u64 * src_h as u64) / dst_h as u64) as usize;
        let sy = sy.min(src_h - 1);
        for dx in 0..dst_w {
            let sx = ((dx as u64 * src_w as u64) / dst_w as u64) as usize;
            let sx = sx.min(src_w - 1);
            let src_idx = (sy * src_w + sx) * 3;
            let r = src_rgb[src_idx];
            let g = src_rgb[src_idx + 1];
            let b = src_rgb[src_idx + 2];
            let argb = 0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            unsafe { core::ptr::write_volatile(dst.add(dy * dst_w + dx), argb); }
        }
    }
}
