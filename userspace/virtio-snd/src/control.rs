//! Control queue operations — virtio-snd PCM lifecycle (§5.14.6.6).
//!
//! Each control request is a 2-descriptor chain on VQ_CONTROL:
//!   desc[0] = request struct (OUT, driver→device)
//!   desc[1] = status response (IN, device→driver, 4 bytes = SndHdr.code)

use crate::proto;
use alloc::format;
use cluu_virtio_core::dma::DmaPool;
use cluu_virtio_core::transport::Transport;
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use libcluu::{debug_print, Error, Result};

/// Submit a control request and spin-wait for the reply.
/// Returns the status code from the device (S_OK = 0x8000 on success).
fn control_request(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    req_bytes: &[u8],
) -> Result<u32> {
    let req_region = pool.alloc(req_bytes.len(), 4)?;
    let status_region = pool.alloc(4, 4)?;

    unsafe {
        core::ptr::copy_nonoverlapping(
            req_bytes.as_ptr(),
            req_region.virt as *mut u8,
            req_bytes.len(),
        );
        core::ptr::write_bytes(status_region.virt as *mut u8, 0, 4);
    }

    let chain = vq.alloc_chain(2).ok_or(Error::Busy)?;
    vq.desc_set(
        chain.head,
        req_region.phys,
        req_bytes.len() as u32,
        VRING_DESC_F_NEXT,
        chain.head + 1,
    );
    vq.desc_set(
        chain.head + 1,
        status_region.phys,
        4,
        VRING_DESC_F_WRITE,
        0,
    );
    vq.submit(chain, 0);
    transport.notify(proto::VQ_CONTROL as u16);

    let mut spins = 0u32;
    loop {
        if let Some((_cookie, _len)) = vq.pop_used() {
            let status =
                unsafe { core::ptr::read_volatile(status_region.virt as *const u32) };
            return Ok(status);
        }
        spins = spins.wrapping_add(1);
        if spins % 1024 == 0 {
            let _ = libcluu::syscall::yield_cpu();
        }
        if spins > 2_000_000 {
            return Err(Error::Timeout);
        }
    }
}

/// Build a bare SndHdr (4 bytes) for simple PCM control ops
/// (prepare/release/start/stop).
fn pcm_simple_request(opcode: u32, stream_id: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&opcode.to_le_bytes());
    buf[4..8].copy_from_slice(&stream_id.to_le_bytes());
    buf
}

pub fn pcm_info(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    stream_id: u32,
) -> Result<u32> {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&proto::R_PCM_INFO.to_le_bytes());
    buf[4..8].copy_from_slice(&stream_id.to_le_bytes());
    buf[8..12].copy_from_slice(&1u32.to_le_bytes());
    buf[12..16].copy_from_slice(&(core::mem::size_of::<proto::SndConfig>() as u32).to_le_bytes());
    control_request(transport, vq, pool, &buf)
}

pub fn pcm_set_params(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    stream_id: u32,
    buffer_bytes: u32,
    period_bytes: u32,
    channels: u8,
    format: u8,
    rate: u8,
) -> Result<u32> {
    let p = proto::PcmSetParams {
        hdr: proto::PcmHdr {
            hdr: proto::SndHdr {
                code: proto::R_PCM_SET_PARAMS,
            },
            stream_id,
        },
        buffer_bytes,
        period_bytes,
        features: 0,
        channels,
        format,
        rate,
        padding: 0,
    };
    let req_bytes = unsafe {
        core::slice::from_raw_parts(
            &p as *const proto::PcmSetParams as *const u8,
            core::mem::size_of::<proto::PcmSetParams>(),
        )
    };
    control_request(transport, vq, pool, req_bytes)
}

pub fn pcm_prepare(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    stream_id: u32,
) -> Result<u32> {
    let buf = pcm_simple_request(proto::R_PCM_PREPARE, stream_id);
    control_request(transport, vq, pool, &buf)
}

pub fn pcm_start(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    stream_id: u32,
) -> Result<u32> {
    let buf = pcm_simple_request(proto::R_PCM_START, stream_id);
    control_request(transport, vq, pool, &buf)
}

pub fn pcm_stop(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    stream_id: u32,
) -> Result<u32> {
    let buf = pcm_simple_request(proto::R_PCM_STOP, stream_id);
    control_request(transport, vq, pool, &buf)
}

pub fn pcm_release(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
    stream_id: u32,
) -> Result<u32> {
    let buf = pcm_simple_request(proto::R_PCM_RELEASE, stream_id);
    control_request(transport, vq, pool, &buf)
}

/// Run the full PCM lifecycle self-test: set_params → prepare → start →
/// stop → release. Emits serial markers at each stage. Uses stream 0
/// (QEMU default: 1 output stream).
pub fn self_test(
    transport: &mut impl Transport,
    vq: &mut Virtqueue,
    pool: &mut DmaPool,
) -> Result<()> {
    let stream_id = 0u32;
    let buffer_bytes = 8192u32;
    let period_bytes = 4096u32;
    let channels = 2u8;
    let format = proto::PCM_FMT_S16;
    let rate = proto::PCM_RATE_44100;

    debug_print("virtio-snd: self_test set_params")?;
    let s = pcm_set_params(
        transport, vq, pool, stream_id, buffer_bytes, period_bytes, channels, format, rate,
    )?;
    debug_print(&format!("virtio-snd: set_params status={:#06x}", s))?;

    debug_print("VIRTIO_SND_PCM_PREPARE")?;
    let s = pcm_prepare(transport, vq, pool, stream_id)?;
    debug_print(&format!("virtio-snd: prepare status={:#06x}", s))?;
    if s != proto::S_OK {
        return Err(Error::InvalidState);
    }

    debug_print("VIRTIO_SND_PCM_START")?;
    let s = pcm_start(transport, vq, pool, stream_id)?;
    debug_print(&format!("virtio-snd: start status={:#06x}", s))?;
    if s != proto::S_OK {
        return Err(Error::InvalidState);
    }

    debug_print("VIRTIO_SND_PCM_STOP")?;
    let s = pcm_stop(transport, vq, pool, stream_id)?;
    debug_print(&format!("virtio-snd: stop status={:#06x}", s))?;

    let s = pcm_release(transport, vq, pool, stream_id)?;
    debug_print(&format!("virtio-snd: release status={:#06x}", s))?;

    Ok(())
}
