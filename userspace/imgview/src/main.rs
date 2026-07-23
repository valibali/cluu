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
    self, extract_reply_id, reply, COMP_FRAME_READY_LABEL, COMP_INPUT_FORWARD_LABEL,
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL,
    COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY, COMP_WIN_SET_PIXEL_REGION_LABEL,
    COMP_WIN_FLAG_NO_CHROME,
};
use libcluu::IpcFlags;
use libcluu::pixel_region::{PixelRegion, GLYPH_H, GLYPH_W};
use libcluu::registry;
use libcluu::syscall::{self, MAP_FRAME_TOKEN};
use libcluu::types::Message;
use libcluu::window_shm::WindowShm;

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const SHM_VA: usize = 0xD000_0000;
const FILE_VA: usize = 0xE000_0000;
const FLAGS_USER_RW: usize = 0x07;

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

    let req_w = 80u16;
    let req_h = 40u16;

    let (win_id, comp_ep, _shm_token, gw, gh, my_ep) = register_window(req_w, req_h)?;
    let _ = libcluu::debug_print(&format!("imgview: window {}x{}", gw, gh));

    let cell_w = gw;
    let cell_h = gh;
    let pixel_w = cell_w as usize * GLYPH_W;
    let pixel_h = cell_h as usize * GLYPH_H;

    let mut region = PixelRegion::alloc(cell_w, cell_h)?;
    let _ = libcluu::debug_print(&format!("imgview: pixel region {}x{} px", pixel_w, pixel_h));

    send_set_pixel_region(comp_ep, win_id, 0, 0, cell_w, cell_h, region.frame_token())?;

    scale_nearest(&rgb, img_w, img_h, &mut region, pixel_w, pixel_h);

    send_damage(comp_ep, win_id, 0, 0, cell_w, cell_h);

    let _ = libcluu::debug_print("imgview: displayed, waiting for input (Esc to quit)");

    let mut recv_buf = [0u8; 256];
    let tokens = [my_ep];
    loop {
        match syscall::ipc_recv_any(&tokens, &mut recv_buf, 60_000) {
            Ok((_idx, len)) => {
                if let Some((msg, _payload)) = ipc::parse_message(&recv_buf[..len]) {
                    if msg.tag.label == COMP_INPUT_FORWARD_LABEL {
                        let kind = msg.words[5] as u32;
                        let scancode = msg.words[3] as u8;
                        if kind == 99 || scancode == 0x01 {
                            let _ = libcluu::debug_print("imgview: exiting");
                            let destroy_msg = Message::new(
                                COMP_WIN_DESTROY_LABEL,
                                [win_id as usize, 0, 0, 0, 0, 0],
                                1,
                            );
                            let _ = ipc::send(comp_ep, &destroy_msg, IpcFlags::empty());
                            region.destroy();
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

fn register_window(
    req_w: u16,
    req_h: u16,
) -> libcluu::Result<(u64, usize, u64, u16, u16, usize)> {
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = syscall::endpoint_create(ipc_cap)?;

    if registry::init("imgview").is_err() {
        return Err(libcluu::Error::InvalidState);
    }

    let comp_ep = registry::lookup_service("compositor:client")
        .ok_or(libcluu::Error::NotFound)?;

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
    let token = reply.words[1] as u64;
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
        space, SHM_VA, token as usize, FLAGS_USER_RW | MAP_FRAME_TOKEN, num_pages, 0,
    )?;

    let fr_msg = Message::new(COMP_FRAME_READY_LABEL, [win_id as usize, 0, 0, 0, 0, 0], 1);
    let _ = ipc::send(my_ep, &fr_msg, IpcFlags::empty());

    Ok((win_id, comp_ep, token, gw, gh, my_ep))
}

fn send_set_pixel_region(
    comp_ep: usize,
    win_id: u64,
    cell_x: u16,
    cell_y: u16,
    cell_w: u16,
    cell_h: u16,
    pixel_token: u64,
) -> libcluu::Result<()> {
    let msg = Message::new(
        COMP_WIN_SET_PIXEL_REGION_LABEL,
        [win_id as usize, cell_x as usize, cell_y as usize, cell_w as usize, cell_h as usize, pixel_token as usize],
        6,
    );
    ipc::send(comp_ep, &msg, IpcFlags::empty())?;
    Ok(())
}

fn send_damage(comp_ep: usize, win_id: u64, x: u16, y: u16, w: u16, h: u16) {
    let dmg = Message::new(
        COMP_WIN_DAMAGE_LABEL,
        [win_id as usize, x as usize, y as usize, w as usize, h as usize, 0],
        5,
    );
    let _ = ipc::send(comp_ep, &dmg, IpcFlags::empty());
}

fn scale_nearest(
    src_rgb: &[u8],
    src_w: usize,
    src_h: usize,
    region: &mut PixelRegion,
    dst_w: usize,
    dst_h: usize,
) {
    let x_ratio = src_w as u64;
    let y_ratio = src_h as u64;

    for dy in 0..dst_h {
        let sy = ((dy as u64 * y_ratio) / dst_w as u64) as usize;
        let sy = sy.min(src_h - 1);
        for dx in 0..dst_w {
            let sx = ((dx as u64 * x_ratio) / dst_w as u64) as usize;
            let sx = sx.min(src_w - 1);
            let src_idx = (sy * src_w + sx) * 3;
            let r = src_rgb[src_idx];
            let g = src_rgb[src_idx + 1];
            let b = src_rgb[src_idx + 2];
            let argb = 0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            region.write_pixel(dx, dy, argb);
        }
    }
}
