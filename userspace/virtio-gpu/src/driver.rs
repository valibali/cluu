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
use libcluu::syscall::{ipc_recv_any_with_sender, yield_cpu};
use libcluu::{debug_print, Error, Result};

use crate::protocol;

/// DMA pool VA base — virtqueue rings + control buffers + framebuffer backing.
const DMA_POOL_VA: usize = 0x5400_0000;
/// DMA pool size: 512 pages (2 MB) — enough for a 640×480×4 framebuffer.
const DMA_POOL_PAGES: usize = 512;

/// MMIO window for the virtio PCI capability BAR.
const MMIO_VA_BASE: usize = 0x5500_0000;

/// Virtqueue size (QEMU virtio-gpu uses 64 for controlq and cursorq).
const QUEUE_SIZE: u16 = 64;

/// Fence timeout — spin iterations before giving up (~2s at ~1µs/iteration).
const FENCE_TIMEOUT_SPINS: u32 = 2_000_000;

/// Response timeout for non-fenced commands (shorter — ~1s).
const CMD_TIMEOUT_SPINS: u32 = 1_000_000;

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
    pub space_token: usize,
    pub next_fence_id: u64,
    pub next_resource_id: u32,
    pub irq_seen: bool,
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
            space_token,
            next_fence_id: 1,
            next_resource_id: 1,
            irq_seen: false,
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
        let cfg: protocol::GpuConfig =
            unsafe { core::ptr::read_volatile(cfg_va as *const protocol::GpuConfig) };

        if cfg.events_read & protocol::VIRTIO_GPU_EVENT_DISPLAY != 0 {
            // Ack by writing the same bit to events_clear.
            unsafe {
                let clear_ptr = (cfg_va + 4) as *mut u32;
                core::ptr::write_volatile(clear_ptr, protocol::VIRTIO_GPU_EVENT_DISPLAY);
            }
            debug_print("virtio-gpu: display event detected and acked")?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Submit a control command and spin-wait for the response.
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
            return Err(Error::Busy); // queue exhaustion
        }

        let resp_size = resp_size.max(core::mem::size_of::<protocol::CtrlHdr>());
        let cmd_region = self.pool.alloc(cmd_bytes.len(), 4)?;
        let resp_region = self.pool.alloc(resp_size, 4)?;

        // Copy command into DMA memory. If fence is requested, set the
        // VIRTIO_GPU_FLAG_FENCE bit and assign a fence_id in the header.
        let fence_id = if fence {
            let fid = self.alloc_fence_id();
            // The header is at the start of the command buffer.
            // We modify the in-memory copy before writing to DMA.
            // Safety: all commands start with CtrlHdr.
            if cmd_bytes.len() >= core::mem::size_of::<protocol::CtrlHdr>() {
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

        let timeout = if fence {
            FENCE_TIMEOUT_SPINS
        } else {
            CMD_TIMEOUT_SPINS
        };
        let mut spins = 0u32;
        loop {
            if let Some((_cookie, _len)) = self.vq_control.pop_used() {
                let resp_type = unsafe {
                    core::ptr::read_volatile(resp_region.virt as *const u32)
                };

                // Verify fence echo if fenced.
                if let Some(expected_fid) = fence_id {
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

                return Ok(resp_type);
            }
            spins = spins.wrapping_add(1);
            if spins % 1024 == 0 {
                let _ = yield_cpu();
            }
            if spins > timeout {
                if fence {
                    debug_print("virtio-gpu: fence timeout")?;
                }
                return Err(Error::Timeout);
            }
        }
    }

    /// Submit ATTACH_BACKING with a scatter-gather list — 3+ descriptor chain:
    /// desc[0] = command (OUT), desc[1..n] = SG entries (OUT), desc[n] = response (IN).
    fn submit_attach_backing(
        &mut self,
        cmd: &protocol::ResourceAttachBacking,
        entries: &[protocol::MemEntry],
    ) -> Result<u32> {
        let n_entries = entries.len();
        let total_descs = 2 + n_entries; // cmd + entries + response
        if self.vq_control.free_capacity() < total_descs as u16 {
            return Err(Error::Busy); // queue exhaustion
        }

        let cmd_region = self.pool.alloc(core::mem::size_of::<protocol::ResourceAttachBacking>(), 4)?;
        let resp_region = self.pool.alloc(core::mem::size_of::<protocol::CtrlHdr>(), 4)?;

        // Allocate one DMA region per SG entry.
        let mut entry_regions: Vec<DmaRegion> = Vec::with_capacity(n_entries);
        for _ in 0..n_entries {
            entry_regions.push(self.pool.alloc(core::mem::size_of::<protocol::MemEntry>(), 4)?);
        }

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

        let mut spins = 0u32;
        loop {
            if let Some((_cookie, _len)) = self.vq_control.pop_used() {
                let resp_type = unsafe {
                    core::ptr::read_volatile(resp_region.virt as *const u32)
                };
                return Ok(resp_type);
            }
            spins = spins.wrapping_add(1);
            if spins % 1024 == 0 {
                let _ = yield_cpu();
            }
            if spins > CMD_TIMEOUT_SPINS {
                return Err(Error::Timeout);
            }
        }
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

        let cmd_region = self.pool.alloc(core::mem::size_of::<protocol::CtrlHdr>(), 4)?;
        let resp_region = self.pool.alloc(resp_size, 8)?;

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

        let mut spins = 0u32;
        loop {
            if let Some((_cookie, _len)) = self.vq_control.pop_used() {
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
                return Ok(DisplayMode {
                    width: 640,
                    height: 480,
                    enabled: false,
                });
            }
            spins = spins.wrapping_add(1);
            if spins % 1024 == 0 {
                let _ = yield_cpu();
            }
            if spins > CMD_TIMEOUT_SPINS {
                return Err(Error::Timeout);
            }
        }
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

        // Check for pending display events before starting.
        if self.check_display_event()? {
            debug_print("virtio-gpu: display event pending at self_test start")?;
        }

        // 1. GET_DISPLAY_INFO
        let mode = self.get_display_info()?;
        let width = mode.width;
        let height = mode.height;
        debug_print(&format!(
            "virtio-gpu: display {}x{} enabled={}",
            width, height, mode.enabled
        ))?;

        // 2. Allocate framebuffer backing (contiguous DMA region).
        let fb_bytes = (width as usize) * (height as usize) * 4;
        let fb_pages = (fb_bytes + 4095) / 4096;
        if fb_pages > DMA_POOL_PAGES - 32 {
            debug_print(&format!(
                "virtio-gpu: framebuffer too large ({} pages), aborting self_test",
                fb_pages
            ))?;
            return Err(Error::BufferTooSmall);
        }
        let backing = self.pool.alloc_contiguous(fb_pages)?;

        // 3. CREATE_2D — B8G8R8X8 format.
        let resource_id = self.alloc_resource_id();
        self.create_2d(resource_id, protocol::VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM, width, height)?;

        // 4. ATTACH_BACKING — single SG entry (contiguous).
        self.attach_backing(resource_id, &backing)?;

        // 5. SET_SCANOUT — bind resource to scanout 0.
        self.set_scanout(0, resource_id, width, height)?;

        // 6. Write test pattern — vertical gradient.
        let fb_ptr = backing.virt as *mut u32;
        for y in 0..height as usize {
            // Linear interpolation between top and bottom colors.
            let t = y as u32 * 256 / height.max(1) as u32;
            let color = lerp_color(TEST_COLOR_TOP, TEST_COLOR_BOT, t);
            for x in 0..width as usize {
                unsafe {
                    *fb_ptr.add(y * width as usize + x) = color;
                }
            }
        }
        debug_print("virtio-gpu: test pattern written")?;

        // 7. TRANSFER_TO_HOST_2D — copy to host (fenced).
        self.transfer_to_host_2d(resource_id, width, height, 0)?;

        // 8. RESOURCE_FLUSH — display (fenced).
        self.resource_flush(resource_id, width, height)?;

        debug_print("VIRTIO_GPU_TEST_PATTERN")?;

        // 9. Check for display events after flush.
        if self.check_display_event()? {
            debug_print("virtio-gpu: display event after flush — re-querying")?;
            let _ = self.get_display_info()?;
        }

        // 10. UNREF_RESOURCE — cleanup.
        // Detach backing first (spec recommends), then unref.
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

    /// Main event loop — listen on IRQ + registry endpoints, drain queues.
    pub fn run_loop(&mut self) -> Result<()> {
        let registry_endpoint = registry::control_endpoint();
        let mut buf = [0u8; 256];
        loop {
            let tokens = [self.irq.endpoint, registry_endpoint];
            let (idx, _len, _sender) = match ipc_recv_any_with_sender(&tokens, &mut buf, 10) {
                Ok(t) => t,
                Err(_) => {
                    self.drain_queues();
                    continue;
                }
            };

            let _ = self.transport.isr_status();
            self.drain_queues();

            if idx == 0 {
                if !self.irq_seen {
                    debug_print("VIRTIO_GPU_IRQ")?;
                    self.irq_seen = true;
                }
                // Check for display events on every IRQ.
                let _ = self.check_display_event();
                self.drain_queues();
                continue;
            }

            // idx == 1: registry message — ignore for now (no IPC clients yet).
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
