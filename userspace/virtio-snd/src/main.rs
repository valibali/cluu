#![no_std]
#![no_main]

//! Virtio-snd audio device driver.
//!
//! P2 scope: PCI discovery, modern transport setup, 4 virtqueues,
//! IRQ attach, registry publish, recv loop. No PCM streaming yet.

extern crate alloc;

mod proto;
mod control;
mod session;

use alloc::collections::BTreeMap;
use alloc::format;

use cluu_virtio_core::pci;
use cluu_virtio_core::transport::{FeatureBits, ModernPciTransport, Transport};
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_WRITE};
use cluu_virtio_core::{DmaPool, DmaRegion, IrqSource};
use libcluu::boot::{
    process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_IPC, TOKEN_SPACE,
};
use libcluu::ipc::{extract_reply_id, AUDIO_CLOSE, AUDIO_OPEN_SESSION, AUDIO_SUBMIT_PCM, AUDIO_TID_CLEANUP, PARAM_DEVICE_PATH};
use libcluu::registry;
use libcluu::syscall::ipc_recv_any_with_sender;
use libcluu::types::Message;
use libcluu::{debug_print, Result};

/// DMA pool for virtqueue rings + small control/event buffers.
const DMA_POOL_VA: usize = 0x5100_0000;
const DMA_POOL_PAGES: usize = 128;

/// MMIO window for the virtio PCI capability BAR.
const MMIO_VA_BASE: usize = 0x5200_0000;

/// Target VA where the caller's PCM page is granted into driver space.
const GRANT_TARGET_VA: usize = 0x5300_0000;

/// Virtqueue size (QEMU virtio-snd uses 64 for all queues).
const QUEUE_SIZE: u16 = 64;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("virtio-snd: error {:?}", e));
            -1
        }
    }
}

struct Driver {
    transport: ModernPciTransport,
    vq_control: Virtqueue,
    vq_event: Virtqueue,
    vq_tx: Virtqueue,
    vq_rx: Virtqueue,
    irq: IrqSource,
    pool: DmaPool,
    space_token: usize,
    _event_buf: DmaRegion,
    irq_seen: bool,
    sessions: BTreeMap<u32, session::AudioSession>,
    next_session_id: u32,
    pending_tx: session::PendingTx,
    next_cookie: u64,
}

fn run() -> Result<()> {
    debug_print("virtio-snd: starting")?;

    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let space_token = info.tokens[TOKEN_SPACE];
    let ipc_token = info.tokens[TOKEN_IPC];

    // ── PCI discovery ────────────────────────────────────────────────────
    if info.params[PARAM_DEVICE_PATH] != 0 {
        let packed = info.params[PARAM_DEVICE_PATH];
        let bus = ((packed >> 16) & 0xFF) as u8;
        let device = ((packed >> 8) & 0xFF) as u8;
        let function = (packed & 0xFF) as u8;
        debug_print(&format!(
            "virtio-snd: init from params BDF={:02x}:{:02x}.{}",
            bus, device, function
        ))?;
    }
    let pci_device = match pci::find_virtio_device_with_params(
        pci_token,
        &[0x1059],
        &[0x1059],
        &info.params,
    ) {
        Ok(d) => d,
        Err(e) => {
            debug_print(&format!("virtio-snd: find_virtio_device failed: {:?}", e))?;
            return Err(e);
        }
    };

    pci::enable_device(pci_token, &pci_device)?;

    let irq_number = pci::get_irq_line(pci_token, &pci_device, &info.params)?;
    debug_print(&format!(
        "VIRTIO_SND_PCI slot={} irq={}",
        pci_device.device, irq_number
    ))?;

    // ── DMA pool ─────────────────────────────────────────────────────────
    let mut pool = DmaPool::new(space_token, DMA_POOL_VA, DMA_POOL_PAGES)?;

    // ── Transport ────────────────────────────────────────────────────────
    let mut transport = ModernPciTransport::new(
        space_token,
        pci_device.clone(),
        pci_device.cap_bar_phys,
        pci_device.cap_bar_size,
        MMIO_VA_BASE,
    )?;

    transport.reset()?;
    let dev_feats = transport.read_device_features()?;
    let want = FeatureBits::VERSION_1.bits() & dev_feats;
    transport.write_driver_features(want)?;

    // Read device config (jacks/streams/chmaps/controls).
    let cfg_va = transport.device_cfg_va;
    let snd_cfg: proto::SndConfig =
        unsafe { core::ptr::read_volatile(cfg_va as *const proto::SndConfig) };
    debug_print(&format!(
        "virtio-snd: config jacks={} streams={} chmaps={} controls={}",
        snd_cfg.jacks, snd_cfg.streams, snd_cfg.chmaps, snd_cfg.controls
    ))?;

    // ── Virtqueues ───────────────────────────────────────────────────────
    let vq_control = Virtqueue::new(&mut pool, QUEUE_SIZE)?;
    let vq_event = Virtqueue::new(&mut pool, QUEUE_SIZE)?;
    let vq_tx = Virtqueue::new(&mut pool, QUEUE_SIZE)?;
    let vq_rx = Virtqueue::new(&mut pool, QUEUE_SIZE)?;

    transport.configure_queue(proto::VQ_CONTROL as u16, &vq_control)?;
    transport.configure_queue(proto::VQ_EVENT as u16, &vq_event)?;
    transport.configure_queue(proto::VQ_TX as u16, &vq_tx)?;
    transport.configure_queue(proto::VQ_RX as u16, &vq_rx)?;

    debug_print("VIRTIO_SND_QUEUES")?;

    // ── IRQ ──────────────────────────────────────────────────────────────
    let irq_token = info.tokens[TOKEN_EXTRA_2];
    let irq = IrqSource::new(ipc_token, irq_token, irq_number)?;

    // ── Post one event buffer so the device can notify us ────────────────
    // The event queue is device→driver: we supply an IN (write) descriptor
    // large enough for one SndEvent (8 bytes). Post BEFORE assembling the
    // array so we still own `vq_event` mutably.
    let event_buf = pool.alloc(core::mem::size_of::<proto::SndEvent>(), 4)?;
    let mut vq_event = vq_event;
    if let Some(chain) = vq_event.alloc_chain(1) {
        vq_event.desc_set(
            chain.head,
            event_buf.phys,
            core::mem::size_of::<proto::SndEvent>() as u32,
            VRING_DESC_F_WRITE,
            0,
        );
        vq_event.submit(chain, 0);
    }

    let mut driver = Driver {
        transport,
        vq_control,
        vq_event,
        vq_tx,
        vq_rx,
        irq,
        pool,
        space_token,
        _event_buf: event_buf,
        irq_seen: false,
        sessions: BTreeMap::new(),
        next_session_id: 1,
        pending_tx: BTreeMap::new(),
        next_cookie: 1,
    };
    driver.transport.set_driver_ok()?;

    {
        let Driver {
            transport,
            vq_control,
            pool,
            ..
        } = &mut driver;
        match control::self_test(transport, vq_control, pool) {
            Ok(()) => debug_print("virtio-snd: control self_test OK")?,
            Err(e) => debug_print(&format!("virtio-snd: control self_test err {:?}", e))?,
        }
    }

    {
        let Driver {
            transport,
            vq_control,
            vq_tx,
            pool,
            pending_tx,
            next_cookie,
            ..
        } = &mut driver;
        match session::self_test(
            transport,
            vq_control,
            vq_tx,
            pool,
            pending_tx,
            next_cookie,
        ) {
            Ok(()) => debug_print("virtio-snd: tx self_test OK")?,
            Err(e) => debug_print(&format!("virtio-snd: tx self_test err {:?}", e))?,
        }
    }

    // ── Registry ─────────────────────────────────────────────────────────
    registry::init("snddev")?;
    let listen_endpoint = info.tokens[TOKEN_EXTRA_0];
    registry::register_output("main", listen_endpoint)?;
    debug_print("virtio-snd: registered as snddev:main")?;
    debug_print("VIRTIO_SND_OK")?;

    // ── Main recv loop ───────────────────────────────────────────────────
    let registry_endpoint = registry::control_endpoint();
    let mut buf = [0u8; 256];
    loop {
        let tokens = [listen_endpoint, driver.irq.endpoint, registry_endpoint];
        let (idx, len, sender_tid) = match ipc_recv_any_with_sender(&tokens, &mut buf, 10) {
            Ok(t) => t,
            Err(_) => {
                driver.drain_queues();
                continue;
            }
        };

        let _ = driver.transport.isr_status();
        driver.drain_queues();

        if idx == 1 {
            let _isr = driver.transport.isr_status();
            if !driver.irq_seen {
                debug_print("VIRTIO_SND_IRQ")?;
                driver.irq_seen = true;
            }
            driver.drain_queues();
            continue;
        }

        if len < core::mem::size_of::<Message>() {
            continue;
        }
        let msg = unsafe { &*(buf.as_ptr() as *const Message) };

        if idx == 2 {
            let payload = &buf[core::mem::size_of::<Message>()..len];
            let _ = registry::handle_incoming_message(msg, payload);
            continue;
        }

        dispatch_audio(&mut driver, msg, sender_tid);
        driver.drain_queues();
    }
}

fn dispatch_audio(driver: &mut Driver, msg: &Message, sender_tid: usize) {
    let reply_token = extract_reply_id(msg);
    match msg.tag.label {
        AUDIO_OPEN_SESSION => {
            let comp_ep = msg.words[0];
            let format = msg.words[1] as u8;
            let rate = msg.words[2] as u8;
            let channels = msg.words[3] as u8;
            let Driver {
                transport,
                vq_control,
                pool,
                sessions,
                next_session_id,
                space_token,
                ..
            } = driver;
            let _ = session::handle_open_session(
                transport,
                vq_control,
                pool,
                sessions,
                next_session_id,
                *space_token,
                GRANT_TARGET_VA,
                comp_ep,
                sender_tid,
                format,
                rate,
                channels,
                reply_token,
            );
        }
        AUDIO_SUBMIT_PCM => {
            let session_id = msg.words[0] as u32;
            let period_id = msg.words[1] as u64;
            let pcm_len = msg.words[2];
            let page_index = msg.words[3];
            let Driver {
                transport,
                vq_tx,
                vq_control,
                pool,
                space_token,
                sessions,
                pending_tx,
                next_cookie,
                ..
            } = driver;
            let _ = session::handle_submit_pcm(
                transport,
                vq_tx,
                vq_control,
                pool,
                *space_token,
                sessions,
                pending_tx,
                next_cookie,
                session_id,
                period_id,
                pcm_len,
                page_index,
            );
        }
        AUDIO_CLOSE => {
            let session_id = msg.words[0] as u32;
            let Driver {
                transport,
                vq_control,
                pool,
                sessions,
                ..
            } = driver;
            let _ = session::handle_close(
                transport,
                vq_control,
                pool,
                sessions,
                session_id,
            );
        }
        AUDIO_TID_CLEANUP => {
            let dead_tid = msg.words[0];
            let Driver {
                transport,
                vq_control,
                pool,
                sessions,
                ..
            } = driver;
            let _ = session::handle_tid_cleanup(
                transport,
                vq_control,
                pool,
                sessions,
                dead_tid,
            );
        }
        _ => {}
    }
}

impl Driver {
    fn drain_queues(&mut self) {
        while let Some((cookie, _len)) = self.vq_tx.pop_used() {
            session::route_completion(&mut self.pending_tx, cookie);
        }
        while self.vq_control.pop_used().is_some() {}
        while self.vq_event.pop_used().is_some() {}
    }
}
