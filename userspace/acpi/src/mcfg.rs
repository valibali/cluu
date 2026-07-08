use crate::tables::{SdtHeader, SdtSignature};

#[derive(Debug, Clone, Copy)]
pub struct McfgEntry {
    pub base_address: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
    pub reserved: u32,
}

#[derive(Debug, Clone)]
pub struct Mcfg {
    pub header: SdtHeader,
    pub entries: alloc::vec::Vec<McfgEntry>,
}

impl Mcfg {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let header = SdtHeader::from_bytes(data)?;
        if header.signature != SdtSignature::MCFG { return None; }
        if data.len() < 44 { return None; }
        let entry_size = 16;
        let entries_start = 44;
        let entries_end = header.length as usize;
        let count = if entries_end > entries_start {
            (entries_end - entries_start) / entry_size
        } else { 0 };
        let mut entries = alloc::vec::Vec::with_capacity(count);
        for i in 0..count {
            let off = entries_start + i * entry_size;
            if off + entry_size > data.len() { break; }
            entries.push(McfgEntry {
                base_address: u64::from_le_bytes([
                    data[off], data[off+1], data[off+2], data[off+3],
                    data[off+4], data[off+5], data[off+6], data[off+7],
                ]),
                segment_group: u16::from_le_bytes([data[off+8], data[off+9]]),
                start_bus: data[off+10],
                end_bus: data[off+11],
                reserved: u32::from_le_bytes([data[off+12], data[off+13], data[off+14], data[off+15]]),
            });
        }
        Some(Self { header, entries })
    }

    pub fn ecam_base(&self) -> Option<u64> {
        self.entries.first().map(|e| e.base_address)
    }
}
