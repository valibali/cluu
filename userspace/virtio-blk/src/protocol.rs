//! virtio-blk on-the-wire request layout (virtio 1.2 §5.2.6).

pub const VIRTIO_BLK_T_IN: u32 = 0; // device → driver (read)
pub const VIRTIO_BLK_T_OUT: u32 = 1; // driver → device (write)
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

pub const SECTOR_SIZE: usize = 512;

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct VirtioBlkReqHeader {
    pub type_: u32,
    pub reserved: u32,
    pub sector: u64,
}
