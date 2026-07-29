//! virtio-gpu 2D driver — PCI init, feature negotiation, virtqueue setup,
//! command submission with fence support, event processing, and self-test.
//!
//! Pattern follows virtio-snd: PCI discovery → modern transport → feature
//! negotiation → virtqueue setup → IRQ → self-test → registry publish.
//!
//! Classic 2D only. No virgl, blobs, or cursor commands.

use alloc::format;
use alloc::vec::Vec;

use cluu_virtio_core::dma::{DmaPool, DmaRegion};
use cluu_virtio_core::pci;
use cluu_virtio_core::transport::{FeatureBits, ModernPciTransport, Transport};
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use cluu_virtio_core::IrqSource;
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_IPC, TOKEN_SPACE};
use libcluu::ipc::PARAM_DEVICE_PATH;
use libcluu::registry;
use libcluu::ipc::{extract_reply_id, parse_message, reply};
use libcluu::syscall::{
    ipc_recv_any_with_sender, space_grant, space_map_range, space_unmap, virt_to_phys, yield_cpu,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

const PAGE_SIZE: usize = 4096;

const GPU_PROBE: u32 = 0x700;
const GPU_GET_DISPLAY_INFO: u32 = 0x701;
const GPU_CREATE_2D: u32 = 0x702;
const GPU_ATTACH_BACKING: u32 = 0x703;
const GPU_SET_SCANOUT: u32 = 0x704;
const GPU_TRANSFER_FLUSH: u32 = 0x705;
const GPU_UNREF_RESOURCE: u32 = 0x706;
const GPU_POLL_EVENT: u32 = 0x707;
const GPU_RESIZE: u32 = 0x708;
const GPU_GRANT_TO_CLIENT: u32 = 0x709;
const GPU_PROTOCOL_VERSION: usize = 1;

const GRANT_TARGET_VA: usize = 0x5600_0000;

use crate::protocol;

/// DMA pool VA base — virtqueue rings + self-test backing.
const DMA_POOL_VA: usize = 0x5400_0000;
const DMA_POOL_PAGES: usize = 64;

/// Command/response transient pool (8 pages, reset per submit).
const CMD_POOL_VA: usize = 0x5300_0000;
const CMD_POOL_PAGES: usize = 8;

/// MMIO window for the virtio PCI capability BAR.
const MMIO_VA_BASE: usize = 0x5500_0000;

/// Framebuffer VA in driver's address space. Separate from the DMA pool so
/// resize can unmap+remap without pool fragmentation. Granted to displayd
/// at GRANT_TARGET_VA.
const DRIVER_FB_VA: usize = 0x5700_0000;

/// Virtqueue size (QEMU virtio-gpu uses 64 for controlq and cursorq).
const QUEUE_SIZE: u16 = 256;

/// Test pattern colors (B8G8R8X8: bytes are B, G, R, X).
const TEST_COLOR_TOP: u32 = 0x00_FF_55_00; // orange
const TEST_COLOR_BOT: u32 = 0x00_00_55_FF; // blue

/// Driver state — owns transport, virtqueues, DMA pool, IRQ.
pub struct GpuDriver {
    pub transport: ModernPciTransport,
    pub vq_control: Virtqueue,
    pub vq_cursor: Virtqueue,
    pub irq: IrqSource,
    pub pool: DmaPool,
    pub cmd_pool: DmaPool,
    pub space_token: usize,
    pub next_fence_id: u64,
    pub next_resource_id: u32,
    pub irq_seen: bool,
    pub fb_virt: usize,
    pub fb_pages: usize,
}

/// Display mode queried via GET_DISPLAY_INFO.
#[derive(Copy, Clone, Default)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub enabled: bool,
}

impl GpuDriver {
    /// Initialize the driver: PCI discovery → transport → features → queues → IRQ.
    pub fn init() -> Result<Self> {
        debug_print("virtio-gpu: starting")?;

        let info = process_info();
        let pci_token = info.tokens[TOKEN_EXTRA_1];
        let space_token = info.tokens[TOKEN_SPACE];
        let ipc_token = info.tokens[TOKEN_IPC];

        if info.params[PARAM_DEVICE_PATH] != 0 {
            let packed = info.params[PARAM_DEVICE_PATH];
            let bus = ((packed >> 16) & 0xFF) as u8;
            let device = ((packed >> 8) & 0xFF) as u8;
            let function = (packed & 0xFF) as u8;
            debug_print(&format!(
                "virtio-gpu: init from params BDF={:02x}:{:02x}.{}",
                bus, device, function
            ))?;
        }

        // ── PCI discovery ────────────────────────────────────────────────
        // Device ID 0x1050 = virtio-gpu modern (non-transitional).
        let pci_device = pci::find_virtio_device_with_params(
            pci_token,
            &[0x1050],
            &[0x1050],
            &info.params,
        )?;

        pci::enable_device(pci_token, &pci_device)?;

        let irq_number = pci::get_irq_line(pci_token, &pci_device, &info.params)?;
        debug_print(&format!(
            "VIRTIO_GPU_PCI slot={} irq={}",
            pci_device.device, irq_number
        ))?;

        // ── DMA pool ─────────────────────────────────────────────────────
        let mut pool = DmaPool::new(space_token, DMA_POOL_VA, DMA_POOL_PAGES)?;
        let cmd_pool = DmaPool::new(space_token, CMD_POOL_VA, CMD_POOL_PAGES)?;

        // ── Transport ────────────────────────────────────────────────────
        let mut transport = ModernPciTransport::new(
            space_token,
            pci_device.clone(),
            pci_device.cap_bar_phys,
            pci_device.cap_bar_size,
            MMIO_VA_BASE,
        )?;

        transport.reset()?;

        // ── Feature negotiation ──────────────────────────────────────────
        // Accept VERSION_1 + optional EDID. Reject VIRGL/UUID/BLOB.
        let dev_feats = transport.read_device_features()?;
        let rejected = dev_feats & protocol::REJECTED_FEATURES;
        if rejected != 0 {
            debug_print(&format!(
                "virtio-gpu: rejecting features {:#x}",
                rejected
            ))?;
        }
        let want = (FeatureBits::VERSION_1.bits() | protocol::VIRTIO_GPU_F_EDID) & dev_feats;
        transport.write_driver_features(want)?;
        debug_print(&format!(
            "virtio-gpu: features dev={:#x} driver={:#x}",
            dev_feats, want
        ))?;

        // ── Read device config ───────────────────────────────────────────
        let cfg_va = transport.device_cfg_va;
        // SAFETY: `cfg_va` is the MMIO-mapped virtio-gpu device config
        // space, established by `ModernPciTransport::new` during BAR
        // mapping. `GpuConfig` is `#[repr(C)]` matching the virtio-gpu
        // config layout. `read_volatile` is required because this is MMIO
        // (the device may update `events_read` at any time). The mapping
        // is valid for the lifetime of `transport`.
        let gpu_cfg: protocol::GpuConfig =
            unsafe { core::ptr::read_volatile(cfg_va as *const protocol::GpuConfig) };
        debug_print(&format!(
            "virtio-gpu: config scanouts={} capsets={} events={:#x}",
            gpu_cfg.num_scanouts, gpu_cfg.num_capsets, gpu_cfg.events_read
        ))?;

        // ── Virtqueues ───────────────────────────────────────────────────
        let vq_control = Virtqueue::new(&mut pool, QUEUE_SIZE)?;
        let vq_cursor = Virtqueue::new(&mut pool, QUEUE_SIZE)?;

        transport.configure_queue(protocol::VQ_CONTROL as u16, &vq_control)?;
        transport.configure_queue(protocol::VQ_CURSOR as u16, &vq_cursor)?;

        debug_print("VIRTIO_GPU_QUEUES")?;

        // ── IRQ ──────────────────────────────────────────────────────────
        let irq_token = info.tokens[TOKEN_EXTRA_2];
        let irq = IrqSource::new(ipc_token, irq_token, irq_number)?;

        let mut driver = GpuDriver {
            transport,
            vq_control,
            vq_cursor,
            irq,
            pool,
            cmd_pool,
            space_token,
            next_fence_id: 1,
            next_resource_id: 1,
            irq_seen: false,
            fb_virt: 0,
            fb_pages: 0,
        };

        driver.transport.set_driver_ok()?;
        debug_print("virtio-gpu: DRIVER_OK set")?;

        Ok(driver)
    }

    /// Allocate the next resource ID (must be non-zero per spec).
    pub fn alloc_resource_id(&mut self) -> u32 {
        let id = self.next_resource_id;
        self.next_resource_id = self.next_resource_id.wrapping_add(1);
        if self.next_resource_id == 0 {
            self.next_resource_id = 1;
        }
        id
    }

    /// Allocate the next fence ID (non-zero for valid fences).
    pub fn alloc_fence_id(&mut self) -> u64 {
        let id = self.next_fence_id;
        self.next_fence_id = self.next_fence_id.wrapping_add(1);
        if self.next_fence_id == 0 {
            self.next_fence_id = 1;
        }
        id
    }

    /// Read device config events_read. If VIRTIO_GPU_EVENT_DISPLAY is set,
    /// ack by writing to events_clear and return true (caller should
    /// re-query GET_DISPLAY_INFO).
    pub fn check_display_event(&mut self) -> Result<bool> {
        let cfg_va = self.transport.device_cfg_va;
        // SAFETY: MMIO read of the device config — same argument as in
        // `init()`. `cfg_va` is a valid MMIO mapping for the lifetime of
        // `self.transport`. `read_volatile` is required for MMIO.
        let cfg: protocol::GpuConfig =
            unsafe { core::ptr::read_volatile(cfg_va as *const protocol::GpuConfig) };

        if cfg.events_read & protocol::VIRTIO_GPU_EVENT_DISPLAY != 0 {
            // Ack by writing the same bit to events_clear.
            // SAFETY: MMIO write to the `events_clear` field at offset 4
            // in the device config region. `cfg_va + 4` is the documented
            // offset of `events_clear` in the virtio-gpu config space.
            // `write_volatile` is required for MMIO writes.
            unsafe {
                let clear_ptr = (cfg_va + 4) as *mut u32;
                core::ptr::write_volatile(clear_ptr, protocol::VIRTIO_GPU_EVENT_DISPLAY);
            }
            debug_print("virtio-gpu: display event detected and acked")?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Wait for a used buffer on the control queue.
    /// QEMU processes 2D commands synchronously during notify, so the
    /// fast path (single pop_used) almost always succeeds immediately.
    fn wait_for_used(&mut self) -> Result<()> {
        if self.vq_control.pop_used().is_some() {
            return Ok(());
        }
        for _ in 0..2000 {
            let _ = yield_cpu();
            if self.vq_control.pop_used().is_some() {
                return Ok(());
            }
        }
        debug_print("virtio-gpu: command timeout")?;
        Err(Error::Timeout)
    }

    /// Submit a control command and wait for the response.
    ///
    /// Generic 2-descriptor chain: desc[0] = command (OUT), desc[1] = response (IN).
    /// The response buffer must be at least `protocol::CtrlHdr` (24 bytes).
    ///
    /// If `fence` is true, sets VIRTIO_GPU_FLAG_FENCE + a unique fence_id in
    /// the command header and verifies the response echoes the same fence_id.
    /// Fence timeout is ≥2s (FENCE_TIMEOUT_SPINS).
    ///
    /// Returns the response type (VIRTIO_GPU_RESP_* value).
    fn submit_command(
        &mut self,
        cmd_bytes: &[u8],
        resp_size: usize,
        fence: bool,
    ) -> Result<u32> {
        if self.vq_control.free_capacity() < 2 {
            return Err(Error::Busy);
        }

        self.cmd_pool.reset();

        let resp_size = resp_size.max(core::mem::size_of::<protocol::CtrlHdr>());
        let cmd_region = self.cmd_pool.alloc(cmd_bytes.len(), 4)?;
        let resp_region = self.cmd_pool.alloc(resp_size, 4)?;

        // Copy command into DMA memory. If fence is requested, set the
        // VIRTIO_GPU_FLAG_FENCE bit and assign a fence_id in the header.
        let fence_id = if fence {
            let fid = self.alloc_fence_id();
            // The header is at the start of the command buffer.
            // We modify the in-memory copy before writing to DMA.
            // Safety: all commands start with CtrlHdr.
            if cmd_bytes.len() >= core::mem::size_of::<protocol::CtrlHdr>() {
                // SAFETY: `cmd_region.virt` is a DMA allocation from
                // `pool.alloc` with 4-byte alignment and size >=
                // `cmd_bytes.len()`. The check above ensures
                // `cmd_bytes.len() >= size_of::<CtrlHdr>()`, so the
                // `*hdr` dereference is within bounds. The copy is
                // non-overlapping (DMA region ≠ cmd_bytes source).
                unsafe {
                    let hdr = cmd_region.virt as *mut protocol::CtrlHdr;
                    core::ptr::copy_nonoverlapping(
                        cmd_bytes.as_ptr(),
                        hdr as *mut u8,
                        cmd_bytes.len(),
                    );
                    (*hdr).flags |= protocol::VIRTIO_GPU_FLAG_FENCE;
                    (*hdr).fence_id = fid;
                }
            } else {
                // SAFETY: `cmd_region.virt` is a DMA allocation with size
                // >= `cmd_bytes.len()` (guaranteed by `pool.alloc`).
                // Non-overlapping source/dest.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        cmd_bytes.as_ptr(),
                        cmd_region.virt as *mut u8,
                        cmd_bytes.len(),
                    );
                }
            }
            Some(fid)
        } else {
            // SAFETY: Same DMA copy as above — `cmd_region.virt` is a
            // valid DMA region of size >= `cmd_bytes.len()`, 4-byte
            // aligned, non-overlapping with the source.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    cmd_bytes.as_ptr(),
                    cmd_region.virt as *mut u8,
                    cmd_bytes.len(),
                );
            }
            None
        };

        // Zero the response buffer.
        // SAFETY: `resp_region.virt` is a DMA allocation with size >=
        // `resp_size` (guaranteed by `pool.alloc`). `write_bytes` fills
        // the region with zeros — no read of uninitialized memory.
        unsafe {
            core::ptr::write_bytes(resp_region.virt as *mut u8, 0, resp_size);
        }

        // Build 2-descriptor chain: cmd (OUT) → response (IN).
        // chain.head = desc[0] (cmd), chain.tail = desc[1] (response).
        let chain = self.vq_control.alloc_chain(2).ok_or(Error::Busy)?;
        let resp_idx = chain.tail;
        self.vq_control.desc_set(
            chain.head,
            cmd_region.phys,
            cmd_bytes.len() as u32,
            VRING_DESC_F_NEXT,
            resp_idx,
        );
        self.vq_control.desc_set(
            resp_idx,
            resp_region.phys,
            resp_size as u32,
            VRING_DESC_F_WRITE,
            0,
        );
        self.vq_control.submit(chain, 0);
        self.transport.notify(protocol::VQ_CONTROL as u16);

        self.wait_for_used()?;

        // SAFETY: `resp_region.virt` is a DMA buffer of size >=
        // `resp_size` >= `size_of::<u32>()`. The device wrote the
        // response type at offset 0. `read_volatile` is used
        // because the buffer was just written by the device via
        // DMA (memory may be WC — volatile ensures the read is
        // not optimized away).
        let resp_type = unsafe {
            core::ptr::read_volatile(resp_region.virt as *const u32)
        };

        // Verify fence echo if fenced.
        if let Some(expected_fid) = fence_id {
            // SAFETY: `resp_region.virt + 8` is within the DMA
            // buffer (size >= `size_of::<CtrlHdr>()` = 24 bytes,
            // and offset 8 is the `fence_id` field). `read_volatile`
            // for the same DMA reason as above.
            let resp_fid = unsafe {
                core::ptr::read_volatile(
                    (resp_region.virt + 8) as *const u64,
                )
            };
            if resp_fid != expected_fid {
                debug_print(&format!(
                    "virtio-gpu: fence mismatch expected={} got={}",
                    expected_fid, resp_fid
                ))?;
                return Err(Error::InvalidState);
            }
        }

        Ok(resp_type)
    }

    /// Submit ATTACH_BACKING with a scatter-gather list — 3+ descriptor chain:
    /// desc[0] = command (OUT), desc[1..n] = SG entries (OUT), desc[n] = response (IN).
    fn submit_attach_backing(
        &mut self,
        cmd: &protocol::ResourceAttachBacking,
        entries: &[protocol::MemEntry],
    ) -> Result<u32> {
        let n_entries = entries.len();
        let total_descs = 2 + n_entries;
        if self.vq_control.free_capacity() < total_descs as u16 {
            return Err(Error::Busy);
        }

        self.cmd_pool.reset();

        let cmd_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::ResourceAttachBacking>(), 4)?;
        let resp_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::CtrlHdr>(), 4)?;

        let mut entry_regions: Vec<DmaRegion> = Vec::with_capacity(n_entries);
        for _ in 0..n_entries {
            entry_regions.push(self.cmd_pool.alloc(core::mem::size_of::<protocol::MemEntry>(), 4)?);
        }

        // SAFETY: All three copies write to DMA regions allocated by
        // `pool.alloc` with 4-byte alignment and sizes matching the
        // source types. `cmd_region` holds exactly
        // `size_of::<ResourceAttachBacking>()` bytes; each `entry_regions[i]`
        // holds `size_of::<MemEntry>()` bytes. The `resp_region` zero-fill
        // covers `size_of::<CtrlHdr>()` bytes. Source pointers are stack
        // locals (`&cmd`, `entry`) — properly aligned, non-overlapping
        // with DMA destinations.
        unsafe {
            core::ptr::copy_nonoverlapping(
                cmd as *const protocol::ResourceAttachBacking as *const u8,
                cmd_region.virt as *mut u8,
                core::mem::size_of::<protocol::ResourceAttachBacking>(),
            );
            for (i, entry) in entries.iter().enumerate() {
                core::ptr::copy_nonoverlapping(
                    entry as *const protocol::MemEntry as *const u8,
                    entry_regions[i].virt as *mut u8,
                    core::mem::size_of::<protocol::MemEntry>(),
                );
            }
            core::ptr::write_bytes(
                resp_region.virt as *mut u8,
                0,
                core::mem::size_of::<protocol::CtrlHdr>(),
            );
        }

        let chain = self
            .vq_control
            .alloc_chain(total_descs as u16)
            .ok_or(Error::Busy)?;

        // Walk the chain to get descriptor indices (not necessarily contiguous).
        let chain_indices = self.vq_control.collect_chain(chain.head);
        // chain_indices[0] = cmd, [1..1+n_entries] = SG entries, [last] = response.
        let resp_idx = chain_indices[chain_indices.len() - 1];

        // desc[0] = command (OUT, NEXT)
        self.vq_control.desc_set(
            chain_indices[0],
            cmd_region.phys,
            core::mem::size_of::<protocol::ResourceAttachBacking>() as u32,
            VRING_DESC_F_NEXT,
            chain_indices[1],
        );

        // desc[1..n] = SG entries (OUT, NEXT)
        for i in 0..n_entries {
            let desc_idx = chain_indices[1 + i];
            let next_idx = chain_indices[2 + i];
            self.vq_control.desc_set(
                desc_idx,
                entry_regions[i].phys,
                core::mem::size_of::<protocol::MemEntry>() as u32,
                VRING_DESC_F_NEXT,
                next_idx,
            );
        }

        // desc[last] = response (IN, WRITE)
        self.vq_control.desc_set(
            resp_idx,
            resp_region.phys,
            core::mem::size_of::<protocol::CtrlHdr>() as u32,
            VRING_DESC_F_WRITE,
            0,
        );

        self.vq_control.submit(chain, 0);
        self.transport.notify(protocol::VQ_CONTROL as u16);

        self.wait_for_used()?;

        // SAFETY: `resp_region.virt` is a DMA buffer of size >=
        // `size_of::<CtrlHdr>()` >= 4. `read_volatile` because the
        // device wrote via DMA.
        let resp_type = unsafe {
            core::ptr::read_volatile(resp_region.virt as *const u32)
        };
        Ok(resp_type)
    }

    // ── High-level command wrappers ──────────────────────────────────────

    /// GET_DISPLAY_INFO — query the first enabled scanout's mode.
    pub fn get_display_info(&mut self) -> Result<DisplayMode> {
        self.get_display_info_inline()
    }

    /// Inline GET_DISPLAY_INFO that reads the full response struct.
    fn get_display_info_inline(&mut self) -> Result<DisplayMode> {
        let cmd = protocol::CtrlHdr {
            type_: protocol::VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
            ..Default::default()
        };
        let resp_size = core::mem::size_of::<protocol::RespDisplayInfo>();

        self.cmd_pool.reset();

        let cmd_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::CtrlHdr>(), 4)?;
        let resp_region = self.cmd_pool.alloc(resp_size, 8)?;

        // SAFETY: `cmd_region` is a DMA buffer of size >=
        // `size_of::<CtrlHdr>()`, 4-byte aligned. `resp_region` is size
        // >= `resp_size`, 8-byte aligned (RespDisplayInfo may need it for
        // the pmodes array). `copy_nonoverlapping` from a stack local
        // (`&cmd`) to DMA — non-overlapping. `write_bytes` zeroes the
        // response buffer before the device writes into it.
        unsafe {
            core::ptr::copy_nonoverlapping(
                &cmd as *const protocol::CtrlHdr as *const u8,
                cmd_region.virt as *mut u8,
                core::mem::size_of::<protocol::CtrlHdr>(),
            );
            core::ptr::write_bytes(resp_region.virt as *mut u8, 0, resp_size);
        }

        let chain = self.vq_control.alloc_chain(2).ok_or(Error::Busy)?;
        let resp_idx = chain.tail;
        self.vq_control.desc_set(
            chain.head,
            cmd_region.phys,
            core::mem::size_of::<protocol::CtrlHdr>() as u32,
            VRING_DESC_F_NEXT,
            resp_idx,
        );
        self.vq_control.desc_set(
            resp_idx,
            resp_region.phys,
            resp_size as u32,
            VRING_DESC_F_WRITE,
            0,
        );
        self.vq_control.submit(chain, 0);
        self.transport.notify(protocol::VQ_CONTROL as u16);

        self.wait_for_used()?;

        // SAFETY: `resp_region` is a DMA buffer of size >=
        // `resp_size` = `size_of::<RespDisplayInfo>()`, 8-byte
        // aligned (allocated with align=8 above). `read_volatile`
        // because the device wrote via DMA. The full struct is
        // within bounds because `pool.alloc` guaranteed the size.
        let resp: protocol::RespDisplayInfo = unsafe {
            core::ptr::read_volatile(resp_region.virt as *const protocol::RespDisplayInfo)
        };
        if resp.hdr.type_ != protocol::VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
            debug_print(&format!(
                "virtio-gpu: GET_DISPLAY_INFO bad response {} ({})",
                protocol::resp_name(resp.hdr.type_),
                resp.hdr.type_
            ))?;
            return Err(Error::InvalidState);
        }

        // Find the first enabled scanout.
        for (i, pmode) in resp.pmodes.iter().enumerate() {
            if pmode.enabled != 0 {
                let mode = DisplayMode {
                    width: pmode.r.width,
                    height: pmode.r.height,
                    enabled: true,
                };
                debug_print(&format!(
                    "virtio-gpu: scanout {} {}x{} enabled={}",
                    i, mode.width, mode.height, pmode.enabled
                ))?;
                debug_print("VIRTIO_GPU_DISPLAY_INFO")?;
                return Ok(mode);
            }
        }

        // No enabled scanout — use a default.
        debug_print("virtio-gpu: no enabled scanout, using 640x480")?;
        Ok(DisplayMode {
            width: 640,
            height: 480,
            enabled: false,
        })
    }

    /// CREATE_2D — create a 2D resource.
    pub fn create_2d(
        &mut self,
        resource_id: u32,
        format: u32,
        width: u32,
        height: u32,
    ) -> Result<u32> {
        let cmd = protocol::ResourceCreate2d {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                ..Default::default()
            },
            resource_id,
            format,
            width,
            height,
        };
        let cmd_bytes = unsafe {
            // SAFETY: `&cmd` is a properly-aligned stack local. Casting
            // from `*const ResourceCreate2d` to `*const u8` is sound
            // because the struct is `#[repr(C)]` and the slice length
            // equals `size_of::<ResourceCreate2d>()`. The slice is
            // immediately passed to `submit_command` which copies it to
            // DMA — no alignment issue on the u8 view.
            core::slice::from_raw_parts(
                &cmd as *const protocol::ResourceCreate2d as *const u8,
                core::mem::size_of::<protocol::ResourceCreate2d>(),
            )
        };
        let resp_type = self.submit_command(
            cmd_bytes,
            core::mem::size_of::<protocol::CtrlHdr>(),
            false,
        )?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: CREATE_2D bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        debug_print("VIRTIO_GPU_CREATE_2D")?;
        Ok(resp_type)
    }

    /// ATTACH_BACKING — attach guest memory (scatter-gather) to a resource.
    /// Uses a contiguous DMA region so the SG list has a single entry.
    pub fn attach_backing(
        &mut self,
        resource_id: u32,
        backing: &DmaRegion,
    ) -> Result<u32> {
        let cmd = protocol::ResourceAttachBacking {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                ..Default::default()
            },
            resource_id,
            nr_entries: 1,
        };
        let entry = protocol::MemEntry {
            addr: backing.phys,
            length: backing.len as u32,
            padding: 0,
        };
        let resp_type = self.submit_attach_backing(&cmd, &[entry])?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: ATTACH_BACKING bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        debug_print("VIRTIO_GPU_ATTACH_BACKING")?;
        Ok(resp_type)
    }

    /// ATTACH_BACKING with a multi-entry scatter-gather list.
    /// Used when backing memory spans non-contiguous physical pages.
    pub fn attach_backing_sg(
        &mut self,
        resource_id: u32,
        entries: &[protocol::MemEntry],
    ) -> Result<u32> {
        let cmd = protocol::ResourceAttachBacking {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                ..Default::default()
            },
            resource_id,
            nr_entries: entries.len() as u32,
        };
        let resp_type = self.submit_attach_backing(&cmd, entries)?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: ATTACH_BACKING_SG bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        Ok(resp_type)
    }

    /// TRANSFER_TO_HOST_2D + RESOURCE_FLUSH for a sub-rectangle (dirty rect).
    /// `offset` is the byte offset of (0,0) in the backing store.
    pub fn transfer_flush_rect(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        offset: u64,
    ) -> Result<()> {
        if self.vq_control.free_capacity() < 4 {
            return Err(Error::Busy);
        }

        self.cmd_pool.reset();

        let tc_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::TransferToHost2d>(), 4)?;
        let tr_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::CtrlHdr>(), 4)?;
        let fc_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::ResourceFlush>(), 4)?;
        let fr_region = self.cmd_pool.alloc(core::mem::size_of::<protocol::CtrlHdr>(), 4)?;

        let transfer_cmd = protocol::TransferToHost2d {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
                ..Default::default()
            },
            r: protocol::Rect { x, y, width: w, height: h },
            offset,
            resource_id,
            padding: 0,
        };
        let flush_cmd = protocol::ResourceFlush {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                ..Default::default()
            },
            r: protocol::Rect { x, y, width: w, height: h },
            resource_id,
            padding: 0,
        };

        // SAFETY: all 4 regions are DMA allocations with matching sizes/alignment.
        unsafe {
            core::ptr::copy_nonoverlapping(
                &transfer_cmd as *const protocol::TransferToHost2d as *const u8,
                tc_region.virt as *mut u8,
                core::mem::size_of::<protocol::TransferToHost2d>(),
            );
            core::ptr::copy_nonoverlapping(
                &flush_cmd as *const protocol::ResourceFlush as *const u8,
                fc_region.virt as *mut u8,
                core::mem::size_of::<protocol::ResourceFlush>(),
            );
            core::ptr::write_bytes(tr_region.virt as *mut u8, 0, core::mem::size_of::<protocol::CtrlHdr>());
            core::ptr::write_bytes(fr_region.virt as *mut u8, 0, core::mem::size_of::<protocol::CtrlHdr>());
        }

        let chain1 = self.vq_control.alloc_chain(2).ok_or(Error::Busy)?;
        self.vq_control.desc_set(chain1.head, tc_region.phys, core::mem::size_of::<protocol::TransferToHost2d>() as u32, VRING_DESC_F_NEXT, chain1.tail);
        self.vq_control.desc_set(chain1.tail, tr_region.phys, core::mem::size_of::<protocol::CtrlHdr>() as u32, VRING_DESC_F_WRITE, 0);
        self.vq_control.submit(chain1, 0);

        let chain2 = self.vq_control.alloc_chain(2).ok_or(Error::Busy)?;
        self.vq_control.desc_set(chain2.head, fc_region.phys, core::mem::size_of::<protocol::ResourceFlush>() as u32, VRING_DESC_F_NEXT, chain2.tail);
        self.vq_control.desc_set(chain2.tail, fr_region.phys, core::mem::size_of::<protocol::CtrlHdr>() as u32, VRING_DESC_F_WRITE, 0);
        self.vq_control.submit(chain2, 0);

        self.transport.notify(protocol::VQ_CONTROL as u16);

        self.wait_for_used()?;
        self.wait_for_used()?;

        // SAFETY: response buffers written by device via DMA.
        let tr_type = unsafe { core::ptr::read_volatile(tr_region.virt as *const u32) };
        let fr_type = unsafe { core::ptr::read_volatile(fr_region.virt as *const u32) };
        if !protocol::resp_ok(tr_type) || !protocol::resp_ok(fr_type) {
            return Err(Error::InvalidState);
        }
        Ok(())
    }

    /// DETACH_BACKING — detach backing from a resource.
    pub fn detach_backing(&mut self, resource_id: u32) -> Result<u32> {
        let cmd = protocol::ResourceDetachBacking {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING,
                ..Default::default()
            },
            resource_id,
            padding: 0,
        };
        let cmd_bytes = unsafe {
            // SAFETY: Same pattern as `create_2d` — `&cmd` is an aligned
            // stack local, `#[repr(C)]` struct, slice length =
            // `size_of::<ResourceDetachBacking>()`.
            core::slice::from_raw_parts(
                &cmd as *const protocol::ResourceDetachBacking as *const u8,
                core::mem::size_of::<protocol::ResourceDetachBacking>(),
            )
        };
        let resp_type = self.submit_command(
            cmd_bytes,
            core::mem::size_of::<protocol::CtrlHdr>(),
            false,
        )?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: DETACH_BACKING bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        Ok(resp_type)
    }

    /// SET_SCANOUT — bind a resource to a scanout (or disable with resource_id=0).
    pub fn set_scanout(
        &mut self,
        scanout_id: u32,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> Result<u32> {
        let cmd = protocol::SetScanout {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_SET_SCANOUT,
                ..Default::default()
            },
            r: protocol::Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            scanout_id,
            resource_id,
        };
        let cmd_bytes = unsafe {
            // SAFETY: Same `from_raw_parts` pattern — `&cmd` is an aligned
            // `#[repr(C)]` stack local; slice length = `size_of::<SetScanout>()`.
            core::slice::from_raw_parts(
                &cmd as *const protocol::SetScanout as *const u8,
                core::mem::size_of::<protocol::SetScanout>(),
            )
        };
        let resp_type = self.submit_command(
            cmd_bytes,
            core::mem::size_of::<protocol::CtrlHdr>(),
            false,
        )?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: SET_SCANOUT bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        debug_print("VIRTIO_GPU_SCANOUT")?;
        Ok(resp_type)
    }

    /// TRANSFER_TO_HOST_2D — copy guest resource to host-side buffer.
    /// Uses a fence to synchronize with the host.
    pub fn transfer_to_host_2d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
        offset: u64,
    ) -> Result<u32> {
        let cmd = protocol::TransferToHost2d {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
                ..Default::default()
            },
            r: protocol::Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            offset,
            resource_id,
            padding: 0,
        };
        let cmd_bytes = unsafe {
            // SAFETY: Same pattern — aligned `#[repr(C)]` stack local,
            // slice length = `size_of::<TransferToHost2d>()`.
            core::slice::from_raw_parts(
                &cmd as *const protocol::TransferToHost2d as *const u8,
                core::mem::size_of::<protocol::TransferToHost2d>(),
            )
        };
        let resp_type = self.submit_command(
            cmd_bytes,
            core::mem::size_of::<protocol::CtrlHdr>(),
            true, // fenced
        )?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: TRANSFER_TO_HOST_2D bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        debug_print("VIRTIO_GPU_TRANSFER")?;
        Ok(resp_type)
    }

    /// RESOURCE_FLUSH — flush resource to display.
    /// Uses a fence to ensure the flush completes before returning.
    pub fn resource_flush(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> Result<u32> {
        let cmd = protocol::ResourceFlush {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                ..Default::default()
            },
            r: protocol::Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            resource_id,
            padding: 0,
        };
        let cmd_bytes = unsafe {
            // SAFETY: Same pattern — aligned `#[repr(C)]` stack local,
            // slice length = `size_of::<ResourceFlush>()`.
            core::slice::from_raw_parts(
                &cmd as *const protocol::ResourceFlush as *const u8,
                core::mem::size_of::<protocol::ResourceFlush>(),
            )
        };
        let resp_type = self.submit_command(
            cmd_bytes,
            core::mem::size_of::<protocol::CtrlHdr>(),
            true, // fenced
        )?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: RESOURCE_FLUSH bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        debug_print("VIRTIO_GPU_FLUSH")?;
        Ok(resp_type)
    }

    /// UNREF_RESOURCE — destroy a resource.
    pub fn unref_resource(&mut self, resource_id: u32) -> Result<u32> {
        let cmd = protocol::ResourceUnref {
            hdr: protocol::CtrlHdr {
                type_: protocol::VIRTIO_GPU_CMD_RESOURCE_UNREF,
                ..Default::default()
            },
            resource_id,
            padding: 0,
        };
        let cmd_bytes = unsafe {
            // SAFETY: Same pattern — aligned `#[repr(C)]` stack local,
            // slice length = `size_of::<ResourceUnref>()`.
            core::slice::from_raw_parts(
                &cmd as *const protocol::ResourceUnref as *const u8,
                core::mem::size_of::<protocol::ResourceUnref>(),
            )
        };
        let resp_type = self.submit_command(
            cmd_bytes,
            core::mem::size_of::<protocol::CtrlHdr>(),
            false,
        )?;
        if !protocol::resp_ok(resp_type) {
            debug_print(&format!(
                "virtio-gpu: UNREF_RESOURCE bad response {} ({})",
                protocol::resp_name(resp_type),
                resp_type
            ))?;
            return Err(Error::InvalidState);
        }
        debug_print("VIRTIO_GPU_UNREF")?;
        Ok(resp_type)
    }

    /// Drain used rings — called from the main loop to reclaim descriptors.
    pub fn drain_queues(&mut self) {
        while self.vq_control.pop_used().is_some() {}
        while self.vq_cursor.pop_used().is_some() {}
    }

    /// Run the full 2D lifecycle self-test:
    /// GET_DISPLAY_INFO → CREATE_2D → ATTACH_BACKING → SET_SCANOUT →
    /// write test pattern → TRANSFER_TO_HOST_2D → RESOURCE_FLUSH → UNREF.
    ///
    /// Emits serial markers at each stage. The test pattern is a vertical
    /// gradient (orange→blue) that fills the entire scanout.
    pub fn self_test(&mut self) -> Result<()> {
        debug_print("virtio-gpu: self_test start")?;

        if self.check_display_event()? {
            debug_print("virtio-gpu: display event pending at self_test start")?;
        }

        let mode = self.get_display_info()?;
        debug_print(&format!(
            "virtio-gpu: display {}x{} enabled={}",
            mode.width, mode.height, mode.enabled
        ))?;

        const TEST_W: u32 = 64;
        const TEST_H: u32 = 64;
        let fb_bytes = (TEST_W as usize) * (TEST_H as usize) * 4;
        let fb_pages = (fb_bytes + 4095) / 4096;
        let backing = self.pool.alloc_contiguous(fb_pages)?;

        let resource_id = self.alloc_resource_id();
        self.create_2d(resource_id, protocol::VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM, TEST_W, TEST_H)?;

        self.attach_backing(resource_id, &backing)?;

        self.set_scanout(0, resource_id, TEST_W, TEST_H)?;

        let fb_ptr = backing.virt as *mut u32;
        for y in 0..TEST_H as usize {
            let t = y as u32 * 256 / TEST_H as u32;
            let color = lerp_color(TEST_COLOR_TOP, TEST_COLOR_BOT, t);
            for x in 0..TEST_W as usize {
                unsafe {
                    *fb_ptr.add(y * TEST_W as usize + x) = color;
                }
            }
        }
        debug_print("virtio-gpu: test pattern written")?;

        self.transfer_to_host_2d(resource_id, TEST_W, TEST_H, 0)?;
        self.resource_flush(resource_id, TEST_W, TEST_H)?;

        debug_print("VIRTIO_GPU_TEST_PATTERN")?;

        if self.check_display_event()? {
            debug_print("virtio-gpu: display event after flush — re-querying")?;
            let _ = self.get_display_info()?;
        }

        let _ = self.set_scanout(0, 0, TEST_W, TEST_H);
        let _ = self.detach_backing(resource_id);
        self.unref_resource(resource_id)?;

        debug_print("VIRTIO_GPU_SELFTEST_OK")?;
        Ok(())
    }

    /// Publish the driver service in the registry.
    pub fn publish(&self) -> Result<()> {
        let info = process_info();
        let listen_endpoint = info.tokens[TOKEN_EXTRA_0];
        registry::init("gpudev")?;
        registry::register_output("main", listen_endpoint)?;
        debug_print("virtio-gpu: registered as gpudev:main")?;
        debug_print("VIRTIO_GPU_OK")?;
        Ok(())
    }

    /// Main event loop — IPC dispatch + IRQ handling.
    ///
    /// Listens on: IRQ endpoint (idx 0), listen endpoint (idx 1), registry (idx 2).
    /// IRQ: ack, drain queues, check display events.
    /// IPC: dispatch GPU_* labels, reply to caller.
    pub fn run_loop(&mut self) -> Result<()> {
        let info = process_info();
        let listen_endpoint = info.tokens[TOKEN_EXTRA_0];
        let registry_endpoint = registry::control_endpoint();
        let mut buf = [0u8; 256];
        loop {
            let tokens = [self.irq.endpoint, listen_endpoint, registry_endpoint];
            let (idx, len, _sender) = match ipc_recv_any_with_sender(&tokens, &mut buf, 100) {
                Ok(t) => t,
                Err(_) => {
                    self.drain_queues();
                    continue;
                }
            };

            if idx == 0 {
                let _ = self.transport.isr_status();
                self.drain_queues();
                self.irq.ack()?;
                if !self.irq_seen {
                    debug_print("VIRTIO_GPU_IRQ")?;
                    self.irq_seen = true;
                }
                let _ = self.check_display_event();
                continue;
            }

            let (msg, payload) = match parse_message(&buf[..len]) {
                Some(m) => m,
                None => continue,
            };

            let reply_token = extract_reply_id(&msg).unwrap_or(0);
            let label = msg.tag.label;

            if label == registry::REGISTRY_GRANT_REQUEST_LABEL {
                let _ = registry::handle_incoming_message(&msg, payload);
                self.drain_queues();
                continue;
            }

            match label {
                GPU_PROBE => {
                    let rmsg = Message::new(
                        GPU_PROBE,
                        [0, GPU_PROTOCOL_VERSION, self.space_token, 0, 0, 0],
                        3,
                    );
                    let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                }

                GPU_GET_DISPLAY_INFO => {
                    match self.get_display_info() {
                        Ok(mode) => {
                            let rmsg = Message::new(
                                GPU_GET_DISPLAY_INFO,
                                [0, mode.width as usize, mode.height as usize, mode.enabled as usize, 0, 0],
                                4,
                            );
                            let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        }
                        Err(_) => {
                            let rmsg = Message::new(GPU_GET_DISPLAY_INFO, [1, 0, 0, 0, 0, 0], 1);
                            let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        }
                    }
                }

                GPU_CREATE_2D => {
                    let resource_id = msg.words[0] as u32;
                    let format = msg.words[1] as u32;
                    let width = msg.words[2] as u32;
                    let height = msg.words[3] as u32;
                    let displayd_space_token = msg.words[4];

                    let fb_bytes = (width as usize) * (height as usize) * 4;
                    let fb_pages = (fb_bytes + PAGE_SIZE - 1) / PAGE_SIZE;

                    if space_map_range(self.space_token, DRIVER_FB_VA, 0, 0x03, fb_pages, 0).is_err() {
                        let rmsg = Message::new(GPU_CREATE_2D, [1, 0, 0, 0, 0, 0], 1);
                        let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        continue;
                    }

                    let mut entries: alloc::vec::Vec<protocol::MemEntry> = alloc::vec::Vec::new();
                    let mut i = 0;
                    while i < fb_pages {
                        let va = DRIVER_FB_VA + i * PAGE_SIZE;
                        let phys = match virt_to_phys(self.space_token, va) {
                            Ok(p) => p as u64,
                            Err(_) => break,
                        };
                        let mut seg_len = PAGE_SIZE as u32;
                        while i + 1 < fb_pages {
                            let next_va = DRIVER_FB_VA + (i + 1) * PAGE_SIZE;
                            match virt_to_phys(self.space_token, next_va) {
                                Ok(np) if (np as u64) == phys + seg_len as u64 => {
                                    seg_len += PAGE_SIZE as u32;
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        entries.push(protocol::MemEntry { addr: phys, length: seg_len, padding: 0 });
                        i += 1;
                    }

                    let err = self.create_2d(resource_id, format, width, height)
                        .and_then(|_| self.attach_backing_sg(resource_id, &entries))
                        .and_then(|_| self.set_scanout(0, resource_id, width, height));

                    if let Err(_) = err {
                        let _ = space_unmap(self.space_token, DRIVER_FB_VA, fb_pages);
                        let rmsg = Message::new(GPU_CREATE_2D, [2, 0, 0, 0, 0, 0], 1);
                        let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        continue;
                    }

                    for page_idx in 0..fb_pages {
                        let _ = space_grant(
                            self.space_token,
                            displayd_space_token,
                            DRIVER_FB_VA + page_idx * PAGE_SIZE,
                            GRANT_TARGET_VA + page_idx * PAGE_SIZE,
                            0x02,
                        );
                    }

                    self.fb_virt = DRIVER_FB_VA;
                    self.fb_pages = fb_pages;

                    debug_print(&format!(
                        "virtio-gpu: CREATE_2D {}x{} {} pages → {} SG entries, granted to displayd",
                        width, height, fb_pages, entries.len()
                    ))?;

                    let pitch = width * 4;
                    let rmsg = Message::new(
                        GPU_CREATE_2D,
                        [0, GRANT_TARGET_VA, fb_bytes, pitch as usize, width as usize, height as usize],
                        6,
                    );
                    let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                }

                GPU_TRANSFER_FLUSH => {
                    let resource_id = msg.words[0] as u32;
                    let x = msg.words[1] as u32;
                    let y = msg.words[2] as u32;
                    let w = msg.words[3] as u32;
                    let h = msg.words[4] as u32;
                    let pitch = msg.words[5] as u64;
                    let offset = (y as u64) * pitch + (x as u64) * 4;
                    match self.transfer_flush_rect(resource_id, x, y, w, h, offset) {
                        Ok(_) => {
                            let rmsg = Message::new(GPU_TRANSFER_FLUSH, [0, 0, 0, 0, 0, 0], 1);
                            let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        }
                        Err(_) => {
                            let rmsg = Message::new(GPU_TRANSFER_FLUSH, [1, 0, 0, 0, 0, 0], 1);
                            let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        }
                    }
                }

                GPU_UNREF_RESOURCE => {
                    let resource_id = msg.words[0] as u32;
                    let _ = self.detach_backing(resource_id);
                    match self.unref_resource(resource_id) {
                        Ok(_) => {
                            if self.fb_pages > 0 {
                                let _ = space_unmap(self.space_token, self.fb_virt, self.fb_pages);
                                self.fb_virt = 0;
                                self.fb_pages = 0;
                            }
                            let rmsg = Message::new(GPU_UNREF_RESOURCE, [0, 0, 0, 0, 0, 0], 1);
                            let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        }
                        Err(_) => {
                            let rmsg = Message::new(GPU_UNREF_RESOURCE, [1, 0, 0, 0, 0, 0], 1);
                            let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        }
                    }
                }

                GPU_POLL_EVENT => {
                    let event_flags: usize = match self.check_display_event() {
                        Ok(true) => 1,
                        _ => 0,
                    };
                    let rmsg = Message::new(GPU_POLL_EVENT, [0, event_flags, 0, 0, 0, 0], 2);
                    let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                }

                GPU_RESIZE => {
                    let resource_id = msg.words[0] as u32;
                    let new_w = msg.words[1] as u32;
                    let new_h = msg.words[2] as u32;
                    let displayd_space_token = msg.words[3];

                    let old_pages = self.fb_pages;

                    let _ = self.detach_backing(resource_id);
                    let _ = self.unref_resource(resource_id);
                    if old_pages > 0 {
                        let _ = space_unmap(self.space_token, self.fb_virt, old_pages);
                    }

                    let new_bytes = (new_w as usize) * (new_h as usize) * 4;
                    let new_pages = (new_bytes + PAGE_SIZE - 1) / PAGE_SIZE;

                    if space_map_range(self.space_token, DRIVER_FB_VA, 0, 0x03, new_pages, 0).is_err() {
                        self.fb_virt = 0;
                        self.fb_pages = 0;
                        let rmsg = Message::new(GPU_RESIZE, [1, 0, 0, 0, 0, 0], 1);
                        let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        continue;
                    }

                    let mut entries: alloc::vec::Vec<protocol::MemEntry> = alloc::vec::Vec::new();
                    let mut i = 0;
                    while i < new_pages {
                        let va = DRIVER_FB_VA + i * PAGE_SIZE;
                        let phys = match virt_to_phys(self.space_token, va) {
                            Ok(p) => p as u64,
                            Err(_) => break,
                        };
                        let mut seg_len = PAGE_SIZE as u32;
                        while i + 1 < new_pages {
                            let next_va = DRIVER_FB_VA + (i + 1) * PAGE_SIZE;
                            match virt_to_phys(self.space_token, next_va) {
                                Ok(np) if (np as u64) == phys + seg_len as u64 => {
                                    seg_len += PAGE_SIZE as u32;
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        entries.push(protocol::MemEntry { addr: phys, length: seg_len, padding: 0 });
                        i += 1;
                    }

                    let format = protocol::VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM;
                    let err = self.create_2d(resource_id, format, new_w, new_h)
                        .and_then(|_| self.attach_backing_sg(resource_id, &entries))
                        .and_then(|_| self.set_scanout(0, resource_id, new_w, new_h));

                    if let Err(_) = err {
                        let _ = space_unmap(self.space_token, DRIVER_FB_VA, new_pages);
                        self.fb_virt = 0;
                        self.fb_pages = 0;
                        let rmsg = Message::new(GPU_RESIZE, [2, 0, 0, 0, 0, 0], 1);
                        let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                        continue;
                    }

                    for page_idx in 0..new_pages {
                        let _ = space_grant(
                            self.space_token,
                            displayd_space_token,
                            DRIVER_FB_VA + page_idx * PAGE_SIZE,
                            GRANT_TARGET_VA + page_idx * PAGE_SIZE,
                            0x02,
                        );
                    }

                    self.fb_virt = DRIVER_FB_VA;
                    self.fb_pages = new_pages;

                    debug_print(&format!(
                        "virtio-gpu: RESIZE {}x{} {} pages → {} SG entries",
                        new_w, new_h, new_pages, entries.len()
                    ))?;

                    let pitch = new_w * 4;
                    let rmsg = Message::new(
                        GPU_RESIZE,
                        [0, GRANT_TARGET_VA, new_bytes, pitch as usize, new_w as usize, new_h as usize],
                        6,
                    );
                    let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                }

                GPU_GRANT_TO_CLIENT => {
                    let client_space_token = msg.words[0];
                    let client_target_va = msg.words[1];
                    debug_print(&format!(
                        "virtio-gpu: GRANT_TO_CLIENT space_tok={:#x} va={:#x} fb_virt={:#x} fb_pages={}",
                        client_space_token, client_target_va, self.fb_virt, self.fb_pages
                    ))?;
                    let mut ok = true;
                    if self.fb_pages > 0 {
                        for page_idx in 0..self.fb_pages {
                            if space_grant(
                                self.space_token,
                                client_space_token,
                                self.fb_virt + page_idx * PAGE_SIZE,
                                client_target_va + page_idx * PAGE_SIZE,
                                0x02,
                            )
                            .is_err()
                            {
                                ok = false;
                                break;
                            }
                        }
                    } else {
                        ok = false;
                    }
                    let status: usize = if ok { 0 } else { 1 };
                    let rmsg = Message::new(GPU_GRANT_TO_CLIENT, [status, 0, 0, 0, 0, 0], 1);
                    let _ = reply(reply_token, &rmsg, IpcFlags::empty());
                }

                _ => {}
            }

            self.drain_queues();
        }
    }
}

/// Linear interpolation between two B8G8R8X8 colors.
/// `t` is 0..256 (fixed-point 8.8).
fn lerp_color(a: u32, b: u32, t: u32) -> u32 {
    let t = t.min(256);
    let inv = 256 - t;
    let lerp_channel = |ca: u32, cb: u32| -> u32 {
        (ca * inv + cb * t) / 256
    };
    let b_ch = lerp_channel(a & 0xFF, b & 0xFF);
    let g_ch = lerp_channel((a >> 8) & 0xFF, (b >> 8) & 0xFF);
    let r_ch = lerp_channel((a >> 16) & 0xFF, (b >> 16) & 0xFF);
    // X channel is always 0.
    (r_ch << 16) | (g_ch << 8) | b_ch
}
