#![no_std]
#![no_main]

//! virtio-net frame send/receive driver.
//!
//! Thin init-layer service on virtio-core. Probes PCI for a virtio-net
//! device, sets up RX/TX virtqueues, and exposes a simple IPC surface:
//!
//! - NET_PKT_SEND:     transmit a raw Ethernet frame
//! - NET_REGISTER_RECV: register an endpoint for receive notifications
//! - NET_GET_MAC:      query the device MAC address
//!
//! No TCP/IP stack — just frame I/O.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use cluu_virtio_core::transport::{FeatureBits, ModernPciTransport, Transport};
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_WRITE};
use cluu_virtio_core::{DmaPool, DmaRegion, IrqSource};
use libcluu::boot::{
    process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_IPC, TOKEN_SPACE,
};
use libcluu::ipc::{
    reply_to_sender, reply_to_sender_with_payload, send_msg_with_payload, DEVMGR_REGISTER_LABEL,
    NET_GET_MAC, NET_PKT_RECV, NET_PKT_SEND, NET_REGISTER_RECV, PARAM_DEVICE_PATH,
};
use libcluu::registry;
use libcluu::syscall::{endpoint_create, ipc_recv_any_with_sender};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Result};

// ── virtio-net constants ──────────────────────────────────────────────────

/// Virtio-net header size in modern mode (VERSION_1 negotiated).
/// 12 bytes: flags, gso_type, hdr_len, gso_size, csum_start, csum_offset,
/// num_buffers. QEMU's virtio-net-pci in modern mode uses this layout
/// regardless of MRG_RXBUF negotiation.
const VNET_HDR_SIZE: usize = 12;
/// Max Ethernet frame (dst MAC + src MAC + ethertype + payload, no jumbo).
const ETH_FRAME_MAX: usize = 1514;
/// Total per-buffer size: virtio-net header + max frame.
const BUF_SIZE: usize = VNET_HDR_SIZE + ETH_FRAME_MAX;
/// Pre-allocated RX buffers posted to the receive virtqueue.
const N_RX_BUFS: usize = 32;
/// Pre-allocated TX buffers for in-flight sends.
const N_TX_BUFS: usize = 8;
/// Virtqueue depth (power of 2, must be <= device max).
const QUEUE_SIZE: u16 = 64;

/// VIRTIO_NET_F_MAC (device feature bit 5).
const VIRTIO_NET_F_MAC: u64 = 1 << 5;

// ── address-space carve-outs (distinct from virtio-blk's) ─────────────────

const DMA_POOL_VA: usize = 0x5500_0000;
const DMA_POOL_PAGES: usize = 64;
const MMIO_VA_BASE: usize = 0x5600_0000;

// ── driver state ──────────────────────────────────────────────────────────

struct NetDriver {
    transport: ModernPciTransport,
    rx_vq: Virtqueue,
    tx_vq: Virtqueue,
    rx_bufs: Vec<DmaRegion>,
    tx_bufs: Vec<DmaRegion>,
    tx_free: Vec<usize>,
    mac: [u8; 6],
    recv_endpoint: usize,
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("virtio-net: error {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    debug_print("virtio-net: starting")?;

    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let space_token = info.tokens[TOKEN_SPACE];

    debug_print(&format!(
        "virtio-net: pci_token={} space={}",
        pci_token, space_token
    ))?;

    // (a) Probe PCI for virtio-net: 0x1000 (transitional) / 0x1041 (modern).
    if info.params[PARAM_DEVICE_PATH] != 0 {
        let packed = info.params[PARAM_DEVICE_PATH];
        let bus = ((packed >> 16) & 0xFF) as u8;
        let device = ((packed >> 8) & 0xFF) as u8;
        let function = (packed & 0xFF) as u8;
        debug_print(&format!(
            "virtio-net: init from params BDF={:02x}:{:02x}.{}",
            bus, device, function
        ))?;
    }
    let pci_device = match cluu_virtio_core::pci::find_virtio_device_with_params(
        pci_token,
        &[0x1000, 0x1041],
        &[0x1041],
        &info.params,
    ) {
        Ok(d) => {
            debug_print(&format!("virtio-net: found PCI device {:?}", d))?;
            d
        }
        Err(e) => {
            debug_print(&format!("virtio-net: find_virtio_device failed: {:?}", e))?;
            return Err(e);
        }
    };

    cluu_virtio_core::pci::enable_device(pci_token, &pci_device)?;

    let mut pool = DmaPool::new(space_token, DMA_POOL_VA, DMA_POOL_PAGES)?;

    let bar_phys = pci_device.cap_bar_phys;
    let bar_size = pci_device.cap_bar_size;
    let mut transport = ModernPciTransport::new(
        space_token,
        pci_device.clone(),
        bar_phys,
        bar_size,
        MMIO_VA_BASE,
    )?;

    // (b) Reset, negotiate features: VERSION_1 + MAC only.
    transport.reset()?;
    let dev_feats = transport.read_device_features()?;
    let want = (FeatureBits::VERSION_1.bits() | VIRTIO_NET_F_MAC) & dev_feats;
    transport.write_driver_features(want)?;

    // Read MAC from device config (offset 0, 6 bytes).
    let cfg_va = transport.device_cfg_va;
    let mac = unsafe {
        [
            core::ptr::read_volatile((cfg_va + 0) as *const u8),
            core::ptr::read_volatile((cfg_va + 1) as *const u8),
            core::ptr::read_volatile((cfg_va + 2) as *const u8),
            core::ptr::read_volatile((cfg_va + 3) as *const u8),
            core::ptr::read_volatile((cfg_va + 4) as *const u8),
            core::ptr::read_volatile((cfg_va + 5) as *const u8),
        ]
    };
    debug_print(&format!(
        "virtio-net: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    ))?;

    // (c) Initialize virtqueues: RX (q0), TX (q1). No controlq — we didn't
    // negotiate VIRTIO_NET_F_CTRL_VQ.
    let rx_vq = Virtqueue::new(&mut pool, QUEUE_SIZE)?;
    let tx_vq = Virtqueue::new(&mut pool, QUEUE_SIZE)?;
    transport.configure_queue(0, &rx_vq)?;
    transport.configure_queue(1, &tx_vq)?;
    transport.set_driver_ok()?;

    // Read back device status to verify FEATURES_OK accepted and DRIVER_OK set.
    let status = unsafe { core::ptr::read_volatile((transport.common_va + 0x14) as *const u8) };
    let link_status = unsafe { core::ptr::read_volatile((transport.device_cfg_va + 6) as *const u16) };
    debug_print(&format!(
        "virtio-net: device_status=0x{:02x} link_status=0x{:04x}",
        status, link_status
    ))?;

    // Pre-allocate RX and TX buffers from the DMA pool.
    let mut rx_bufs: Vec<DmaRegion> = Vec::with_capacity(N_RX_BUFS);
    let mut tx_bufs: Vec<DmaRegion> = Vec::with_capacity(N_TX_BUFS);
    for _ in 0..N_RX_BUFS {
        rx_bufs.push(pool.alloc(BUF_SIZE, 64)?);
    }
    for _ in 0..N_TX_BUFS {
        tx_bufs.push(pool.alloc(BUF_SIZE, 64)?);
    }
    let tx_free: Vec<usize> = (0..N_TX_BUFS).collect();

    let mut driver = NetDriver {
        transport,
        rx_vq,
        tx_vq,
        rx_bufs,
        tx_bufs,
        tx_free,
        mac,
        recv_endpoint: 0,
    };

    // Post all RX buffers to the receive virtqueue.
    for i in 0..N_RX_BUFS {
        driver.post_rx(i)?;
    }
    driver.transport.notify(0);

    // (d) Read PCI Interrupt Line and attach IRQ handler.
    let irq_number = cluu_virtio_core::pci::get_irq_line(pci_token, &pci_device, &info.params)?;
    debug_print(&format!(
        "virtio-net: PCI Interrupt Line = {}",
        irq_number
    ))?;

    let irq_token = info.tokens[TOKEN_EXTRA_2];
    let ipc_token = info.tokens[TOKEN_IPC];
    let irq = IrqSource::new(ipc_token, irq_token, irq_number)?;
    debug_print(&format!(
        "virtio-net: IRQ attached (endpoint={} irq={})",
        irq.endpoint, irq.irq_number
    ))?;

    // (e) Register with the registry as "netdev".
    registry::init("netdev")?;
    let listen_endpoint = info.tokens[TOKEN_EXTRA_0];
    let listen_endpoint = if listen_endpoint != 0 {
        listen_endpoint
    } else {
        endpoint_create(pci_token)?
    };
    registry::register_output("main", listen_endpoint)?;
    debug_print("virtio-net: registered as netdev:main")?;

    let registry_endpoint = registry::control_endpoint();
    register_with_devmgr(&driver.mac);

    // (f) Main service loop: listen + irq + registry on a single thread.
    // IRQ-driven with 50ms fallback poll: on the `pc` (i440FX) machine type,
    // PCI INTx routes through the 8259 PIC which the kernel programs. The
    // 50ms timeout is a safety net — if IRQ delivery ever misses, frames
    // still get drained. Most of the time the thread blocks in recv (CPU
    // halted) and wakes on IRQ or IPC.
    let mut buf = [0u8; 4096];
    loop {
        let tokens = [listen_endpoint, irq.endpoint, registry_endpoint];
        let (idx, len, _sender) = match ipc_recv_any_with_sender(&tokens, &mut buf, 50) {
            Ok(t) => t,
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {
                let _isr = driver.transport.isr_status();
                driver.drain_rx();
                driver.drain_tx();
                continue;
            }
            Err(_) => continue,
        };

        if idx == 1 {
            let _isr = driver.transport.isr_status();
            driver.drain_rx();
            driver.drain_tx();
            continue;
        }

        if let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) {
            if idx == 2 {
                let _ = registry::handle_incoming_message(&msg, payload);
                continue;
            }

            handle_request(&mut driver, &msg, payload);
        }
    }
}

impl NetDriver {
    /// Post one RX buffer to the receive virtqueue. The descriptor is
    /// writable (device fills it). Cookie = buffer index.
    fn post_rx(&mut self, buf_idx: usize) -> Result<()> {
        let chain = self
            .rx_vq
            .alloc_chain(1)
            .ok_or(libcluu::Error::Busy)?;
        let region = &self.rx_bufs[buf_idx];
        self.rx_vq.desc_set(
            chain.head,
            region.phys,
            BUF_SIZE as u32,
            VRING_DESC_F_WRITE,
            0,
        );
        self.rx_vq.submit(chain, buf_idx as u64);
        Ok(())
    }

    /// Drain the RX used ring. For each completed buffer, deliver the
    /// received frame to the registered endpoint, then re-post the buffer.
    fn drain_rx(&mut self) {
        let mut count = 0u32;
        while let Some((cookie, written)) = self.rx_vq.pop_used() {
            count += 1;
            let buf_idx = cookie as usize;
            if buf_idx >= self.rx_bufs.len() {
                continue;
            }
            let frame_len = written as usize;
            if frame_len <= VNET_HDR_SIZE {
                let _ = self.post_rx(buf_idx);
                continue;
            }
            let data_len = frame_len - VNET_HDR_SIZE;
            let data_start = self.rx_bufs[buf_idx].virt + VNET_HDR_SIZE;
            if self.recv_endpoint != 0 {
                let frame =
                    unsafe { core::slice::from_raw_parts(data_start as *const u8, data_len) };
                let msg = Message::new(NET_PKT_RECV, [data_len, 0, 0, 0, 0, 0], 1);
                let _ = send_msg_with_payload(self.recv_endpoint, &msg, frame);
            }
            let _ = self.post_rx(buf_idx);
        }
        if count > 0 {
            let _ = debug_print(&format!("virtio-net: drain_rx {} frames", count));
        }
        self.transport.notify(0);
    }

    /// Drain the TX used ring. Return completed TX buffers to the free-list.
    fn drain_tx(&mut self) {
        let mut count = 0u32;
        while let Some((cookie, _)) = self.tx_vq.pop_used() {
            count += 1;
            let buf_idx = cookie as usize;
            if buf_idx < self.tx_bufs.len() {
                self.tx_free.push(buf_idx);
            }
        }
        if count > 0 {
            let _ = debug_print(&format!("virtio-net: drain_tx {} completions, tx_free={}", count, self.tx_free.len()));
        }
    }

    /// Transmit a frame. Copies the frame data into a pre-allocated TX
    /// buffer (zeroed virtio-net header prepended), submits to the TX
    /// virtqueue, and notifies the device.
    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        if frame.len() > ETH_FRAME_MAX {
            return Err(libcluu::Error::InvalidArgument);
        }
        let buf_idx = self.tx_free.pop().ok_or(libcluu::Error::Busy)?;
        let region = &self.tx_bufs[buf_idx];

        // Zero the virtio-net header, then copy the frame after it.
        unsafe {
            core::ptr::write_bytes(region.virt as *mut u8, 0, VNET_HDR_SIZE);
            let dst = (region.virt + VNET_HDR_SIZE) as *mut u8;
            core::ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
        }

        let total = VNET_HDR_SIZE + frame.len();
        let chain = self
            .tx_vq
            .alloc_chain(1)
            .ok_or(libcluu::Error::Busy)?;
        self.tx_vq
            .desc_set(chain.head, region.phys, total as u32, 0, 0);
        self.tx_vq.submit(chain, buf_idx as u64);
        self.transport.notify(1);
        if frame.len() >= 14 {
            let dst = &frame[..6];
            let src = &frame[6..12];
            let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
            let _ = debug_print(&format!(
                "virtio-net: TX dst={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} src={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} eth=0x{:04x} len={}",
                dst[0], dst[1], dst[2], dst[3], dst[4], dst[5],
                src[0], src[1], src[2], src[3], src[4], src[5],
                ethertype, frame.len()
            ));
        }
        Ok(())
    }
}

fn handle_request(driver: &mut NetDriver, msg: &Message, payload: &[u8]) {
    match msg.tag.label {
        NET_PKT_SEND => {
            let frame_len = msg.words[0];
            if frame_len > payload.len() || frame_len > ETH_FRAME_MAX {
                send_error_reply(msg, -1);
                return;
            }
            driver.drain_tx();
            let frame = &payload[..frame_len];
            match driver.send_frame(frame) {
                Ok(()) => {
                    let reply_msg = Message::new(NET_PKT_SEND, [0, frame_len, 0, 0, 0, 0], 2);
                    let _ = reply_to_sender(msg, &reply_msg, 0, IpcFlags::empty());
                }
                Err(e) => send_error_reply(msg, e.to_errno()),
            }
        }

        NET_REGISTER_RECV => {
            driver.recv_endpoint = msg.words[0];
            let reply_msg = Message::new(NET_REGISTER_RECV, [0, 0, 0, 0, 0, 0], 1);
            let _ = reply_to_sender(msg, &reply_msg, 0, IpcFlags::empty());
        }

        NET_GET_MAC => {
            let reply_msg = Message::new(NET_GET_MAC, [0, 6, 0, 0, 0, 0], 2);
            let _ = reply_to_sender_with_payload(msg, &reply_msg, &driver.mac, 0);
        }

        _ => {}
    }
}

fn register_with_devmgr(mac: &[u8; 6]) {
    let devmgr_ep = match registry::subscribe_output("devmgr", "main") {
        Ok(ep) => ep,
        Err(e) => {
            let _ = debug_print(&format!(
                "virtio-net: devmgr subscribe failed {:?} — continuing without registration",
                e
            ));
            return;
        }
    };
    let mut msg = Message::new(
        DEVMGR_REGISTER_LABEL,
        [mac[0] as usize | ((mac[1] as usize) << 8), 0, 0, 0, 0, 0],
        1,
    );
    match libcluu::ipc::call(devmgr_ep, &mut msg, IpcFlags::empty()) {
        Ok(()) => {
            let _ = debug_print(&format!(
                "virtio-net: registered with devmgr (status={})",
                msg.words[0]
            ));
        }
        Err(e) => {
            let _ = debug_print(&format!("virtio-net: devmgr register failed {:?}", e));
        }
    }
}

fn send_error_reply(msg: &Message, code: isize) {
    let reply_msg = Message::new(0, [code as usize, 0, 0, 0, 0, 0], 1);
    let _ = reply_to_sender(msg, &reply_msg, 0, IpcFlags::empty());
}
