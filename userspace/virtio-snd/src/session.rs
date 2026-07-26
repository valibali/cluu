//! Per-session state + TX queue PCM streaming.
//!
//! Each app gets one AudioSession. The driver runs the virtio-snd control
//! lifecycle (set_params → prepare → start) at open, and (stop → release)
//! at close. Between open and close, AUDIO_SUBMIT_PCM reads PCM from a
//! pre-granted shared page (mapped at GRANT_TARGET_VA at session open).
//! The driver translates the granted VA to a physical address and builds
//! a 3-descriptor TX chain:
//!   desc[0] = PcmXfer  (4B, OUT)
//!   desc[1] = PCM data (≤4096B, OUT, from granted page)
//!   desc[2] = PcmStatus (8B, IN/WRITE)
//!
//! Ring buffer: N grant pages are mapped contiguously at GRANT_TARGET_VA.
//! Each submit specifies a page_index (0..N-1). The driver pre-allocates
//! N xfer + N status DMA regions so each slot has its own TX chain headers.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;

use crate::proto;
use cluu_virtio_core::dma::{DmaPool, DmaRegion};
use cluu_virtio_core::transport::Transport;
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use libcluu::ipc::{reply, AUDIO_COMPLETE, AUDIO_OPEN_SESSION};
use libcluu::syscall::virt_to_phys;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

pub const PERIOD_BYTES: usize = 2048;
pub const BUFFER_BYTES: u32 = 8192;
const MAX_SESSIONS: usize = 1;
const SELF_TEST_PERIODS: usize = 16;
const SELF_TEST_SESSION: u32 = 0xFFFF_FFFF;

/// Number of ring slots (grant pages) per session.
pub const RING_SLOTS: usize = 8;

pub struct AudioSession {
    pub stream_id: u32,
    pub completion_endpoint: usize,
    pub owner_tid: usize,
    pub format: u8,
    pub rate: u8,
    pub channels: u8,
    pub grant_target_va: usize,
    pub xfer_regions: Vec<DmaRegion>,
    pub status_regions: Vec<DmaRegion>,
    pub started: bool,
    pub slot_in_flight: [bool; RING_SLOTS],
}

/// Cookie → (session_id, period_id, completion_endpoint, page_index). Populated on
/// submit, consumed when the TX used ring yields the cookie.
pub type PendingTx = BTreeMap<u64, (u32, u64, usize, usize)>;

pub fn handle_open_session(
    transport: &mut impl Transport,
    vq_ctrl: &mut Virtqueue,
    pool: &mut DmaPool,
    sessions: &mut BTreeMap<u32, AudioSession>,
    next_session_id: &mut u32,
    driver_space_token: usize,
    grant_target_va: usize,
    completion_endpoint: usize,
    owner_tid: usize,
    format: u8,
    rate: u8,
    channels: u8,
    reply_token: Option<usize>,
) -> Result<()> {
    let sid = *next_session_id;
    *next_session_id = next_session_id.wrapping_add(1);
    if *next_session_id == SELF_TEST_SESSION {
        *next_session_id = 0;
    }

    let stream_id = 0u32;

    if sessions.len() >= MAX_SESSIONS {
        let rmsg = Message::new(AUDIO_OPEN_SESSION, [2, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
        return Err(Error::Busy);
    }

    let s = crate::control::pcm_set_params(
        transport,
        vq_ctrl,
        pool,
        stream_id,
        BUFFER_BYTES,
        PERIOD_BYTES as u32,
        channels,
        format,
        rate,
    )?;
    debug_print(&format!(
        "virtio-snd: session_open set_params rate={} ch={} fmt={} status={:#06x}",
        rate, channels, format, s
    ))?;
    if s != proto::S_OK {
        let rmsg = Message::new(AUDIO_OPEN_SESSION, [1, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
        return Err(Error::InvalidState);
    }

    let s = crate::control::pcm_prepare(transport, vq_ctrl, pool, stream_id)?;
    if s != proto::S_OK {
        return Err(Error::InvalidState);
    }

    let mut xfer_regions = Vec::with_capacity(RING_SLOTS);
    let mut status_regions = Vec::with_capacity(RING_SLOTS);
    for _ in 0..RING_SLOTS {
        xfer_regions.push(pool.alloc(core::mem::size_of::<proto::PcmXfer>(), 4)?);
        status_regions.push(pool.alloc(core::mem::size_of::<proto::PcmStatus>(), 4)?);
    }

    sessions.insert(
        sid,
        AudioSession {
            stream_id,
            completion_endpoint,
            owner_tid,
            format,
            rate,
            channels,
            grant_target_va,
            xfer_regions,
            status_regions,
            started: false,
            slot_in_flight: [false; RING_SLOTS],
        },
    );

    let rmsg = Message::new(
        AUDIO_OPEN_SESSION,
        [0, sid as usize, driver_space_token, grant_target_va, 0, 0],
        4,
    );
    if let Some(rt) = reply_token {
        let _ = reply(rt, &rmsg, IpcFlags::empty());
    }
    Ok(())
}

pub fn handle_submit_pcm(
    transport: &mut impl Transport,
    vq_tx: &mut Virtqueue,
    vq_ctrl: &mut Virtqueue,
    pool: &mut DmaPool,
    space_token: usize,
    sessions: &mut BTreeMap<u32, AudioSession>,
    pending: &mut PendingTx,
    next_cookie: &mut u64,
    session_id: u32,
    period_id: u64,
    pcm_len: usize,
    page_index: usize,
) -> Result<()> {
    let session = sessions
        .get_mut(&session_id)
        .ok_or(Error::InvalidArgument)?;

    if pcm_len == 0 || pcm_len > PERIOD_BYTES {
        return Err(Error::InvalidArgument);
    }
    if page_index >= RING_SLOTS {
        return Err(Error::InvalidArgument);
    }
    if session.slot_in_flight[page_index] {
        return Err(Error::Busy);
    }

    let pcm_va = session.grant_target_va + page_index * PERIOD_BYTES;
    let pcm_phys = virt_to_phys(space_token, pcm_va)? as u64;

    let xfer = &session.xfer_regions[page_index];
    let status = &session.status_regions[page_index];

    unsafe {
        let x = &mut *(xfer.virt as *mut proto::PcmXfer);
        x.stream_id = session.stream_id;
        core::ptr::write_bytes(status.virt as *mut u8, 0, 8);
    }

    let chain = vq_tx.alloc_chain(3).ok_or(Error::Busy)?;
    vq_tx.desc_set(
        chain.head,
        xfer.phys,
        core::mem::size_of::<proto::PcmXfer>() as u32,
        VRING_DESC_F_NEXT,
        chain.head + 1,
    );
    vq_tx.desc_set(
        chain.head + 1,
        pcm_phys,
        pcm_len as u32,
        VRING_DESC_F_NEXT,
        chain.head + 2,
    );
    vq_tx.desc_set(
        chain.head + 2,
        status.phys,
        core::mem::size_of::<proto::PcmStatus>() as u32,
        VRING_DESC_F_WRITE,
        0,
    );

    let cookie = *next_cookie;
    *next_cookie = next_cookie.wrapping_add(1);
    pending.insert(cookie, (session_id, period_id, session.completion_endpoint, page_index));
    session.slot_in_flight[page_index] = true;
    vq_tx.submit(chain, cookie);
    transport.notify(proto::VQ_TX as u16);

    if !session.started {
        let s = crate::control::pcm_start(transport, vq_ctrl, pool, session.stream_id)?;
        if s == proto::S_OK {
            session.started = true;
        }
    }
    Ok(())
}

pub fn handle_close(
    transport: &mut impl Transport,
    vq_tx: &mut Virtqueue,
    vq_ctrl: &mut Virtqueue,
    pool: &mut DmaPool,
    sessions: &mut BTreeMap<u32, AudioSession>,
    pending: &mut PendingTx,
    session_id: u32,
) -> Result<()> {
    let session = sessions.remove(&session_id).ok_or(Error::InvalidArgument)?;

    // Drain in-flight TX submissions before stop+release (safe close).
    let mut spins = 0u32;
    loop {
        let mut any = false;
        while let Some((cookie, _len)) = vq_tx.pop_used() {
            route_completion(pending, sessions, cookie);
            any = true;
        }
        if !any && session.slot_in_flight.iter().all(|&f| !f) {
            break;
        }
        let _ = transport.isr_status();
        spins = spins.wrapping_add(1);
        if spins % 1024 == 0 {
            let _ = libcluu::syscall::yield_cpu();
        }
        if spins > 2_000_000 {
            debug_print("virtio-snd: close drain timeout")?;
            break;
        }
    }

    let _ = crate::control::pcm_stop(transport, vq_ctrl, pool, session.stream_id);
    let _ = crate::control::pcm_release(transport, vq_ctrl, pool, session.stream_id);
    Ok(())
}

pub fn route_completion(
    pending: &mut PendingTx,
    sessions: &mut BTreeMap<u32, AudioSession>,
    cookie: u64,
) {
    if let Some((sid, pid, comp_ep, page_index)) = pending.remove(&cookie) {
        if sid != SELF_TEST_SESSION {
            if let Some(session) = sessions.get_mut(&sid) {
                let status = if page_index < session.status_regions.len() {
                    let region = &session.status_regions[page_index];
                    let s = unsafe {
                        core::ptr::read_volatile(region.virt as *const proto::PcmStatus)
                    };
                    s.status
                } else {
                    proto::S_OK
                };
                session.slot_in_flight[page_index] = false;
                if comp_ep != 0 {
                    let msg = Message::new(
                        AUDIO_COMPLETE,
                        [pid as usize, status as usize, 0, 0, 0, 0],
                        2,
                    );
                    let _ = libcluu::syscall::ipc_send(comp_ep, msg.as_bytes());
                }
            }
        }
    }
}

pub fn handle_tid_cleanup(
    transport: &mut impl Transport,
    vq_ctrl: &mut Virtqueue,
    pool: &mut DmaPool,
    sessions: &mut BTreeMap<u32, AudioSession>,
    dead_tid: usize,
) {
    let to_drop: Vec<u32> = sessions
        .iter()
        .filter_map(|(sid, s)| if s.owner_tid == dead_tid { Some(*sid) } else { None })
        .collect();
    for sid in to_drop {
        if let Some(session) = sessions.remove(&sid) {
            let _ = crate::control::pcm_stop(transport, vq_ctrl, pool, session.stream_id);
            let _ = crate::control::pcm_release(transport, vq_ctrl, pool, session.stream_id);
            let _ = debug_print(&format!("virtio-snd: session {} reaped (tid={})", sid, dead_tid));
        }
    }
}

pub fn self_test(
    transport: &mut impl Transport,
    vq_ctrl: &mut Virtqueue,
    vq_tx: &mut Virtqueue,
    pool: &mut DmaPool,
    pending: &mut PendingTx,
    next_cookie: &mut u64,
) -> Result<()> {
    let stream_id = 0u32;

    let s = crate::control::pcm_set_params(
        transport, vq_ctrl, pool, stream_id, BUFFER_BYTES, PERIOD_BYTES as u32,
        2, proto::PCM_FMT_S16, proto::PCM_RATE_44100,
    )?;
    if s != proto::S_OK {
        return Err(Error::InvalidState);
    }
    let s = crate::control::pcm_prepare(transport, vq_ctrl, pool, stream_id)?;
    if s != proto::S_OK {
        return Err(Error::InvalidState);
    }

    let mut xfer_regions = Vec::with_capacity(RING_SLOTS);
    let mut status_regions = Vec::with_capacity(RING_SLOTS);
    for _ in 0..RING_SLOTS {
        xfer_regions.push(pool.alloc(core::mem::size_of::<proto::PcmXfer>(), 4)?);
        status_regions.push(pool.alloc(core::mem::size_of::<proto::PcmStatus>(), 4)?);
    }
    let silence_region = pool.alloc(PERIOD_BYTES, 4)?;
    unsafe {
        core::ptr::write_bytes(silence_region.virt as *mut u8, 0, PERIOD_BYTES);
    }

    let mut sessions = BTreeMap::new();
    sessions.insert(
        SELF_TEST_SESSION,
        AudioSession {
            stream_id,
            completion_endpoint: 0,
            owner_tid: 0,
            format: proto::PCM_FMT_S16,
            rate: proto::PCM_RATE_44100,
            channels: 2,
            grant_target_va: silence_region.virt,
            xfer_regions,
            status_regions,
            started: true,
            slot_in_flight: [false; RING_SLOTS],
        },
    );

    let s = crate::control::pcm_start(transport, vq_ctrl, pool, stream_id)?;
    if s != proto::S_OK {
        return Err(Error::InvalidState);
    }

    let space_token = pool.space_token();
    for i in 0..SELF_TEST_PERIODS {
        handle_submit_pcm(
            transport, vq_tx, vq_ctrl, pool, space_token,
            &mut sessions,
            pending, next_cookie,
            SELF_TEST_SESSION, i as u64, PERIOD_BYTES,
            i % RING_SLOTS,
        )?;
    }

    let mut completed = 0usize;
    let mut spins = 0u32;
    while completed < SELF_TEST_PERIODS {
        while let Some((cookie, _len)) = vq_tx.pop_used() {
            route_completion(pending, &mut sessions, cookie);
            completed += 1;
        }
        if completed >= SELF_TEST_PERIODS {
            break;
        }
        let _isr = transport.isr_status();
        spins = spins.wrapping_add(1);
        if spins % 1024 == 0 {
            let _ = libcluu::syscall::yield_cpu();
        }
        if spins > 5_000_000 {
            debug_print(&format!(
                "virtio-snd: tx self_test timeout ({}/{})",
                completed, SELF_TEST_PERIODS
            ))?;
            return Err(Error::Timeout);
        }
    }

    let _ = crate::control::pcm_stop(transport, vq_ctrl, pool, stream_id);
    let _ = crate::control::pcm_release(transport, vq_ctrl, pool, stream_id);

    debug_print(&format!(
        "virtio-snd: tx self_test {} periods completed",
        completed
    ))?;
    debug_print("VIRTIO_SND_TX_OK")?;
    Ok(())
}
