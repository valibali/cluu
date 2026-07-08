pub const SDT_SIGNATURE_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtSignature(pub [u8; SDT_SIGNATURE_LEN]);

impl SdtSignature {
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).unwrap_or("????")
    }

    pub const fn new(s: [u8; SDT_SIGNATURE_LEN]) -> Self {
        Self(s)
    }

    pub const APIC: SdtSignature = SdtSignature::new(*b"APIC");
    pub const FADT: SdtSignature = SdtSignature::new(*b"FACP");
    pub const MCFG: SdtSignature = SdtSignature::new(*b"MCFG");
    pub const XSDT: SdtSignature = SdtSignature::new(*b"XSDT");
    pub const RSDT: SdtSignature = SdtSignature::new(*b"RSDT");
    pub const DSDT: SdtSignature = SdtSignature::new(*b"DSDT");
}

#[derive(Debug, Clone, Copy)]
pub struct SdtHeader {
    pub signature: SdtSignature,
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

impl SdtHeader {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 36 { return None; }
        let mut sig = [0u8; 4];
        sig.copy_from_slice(&data[0..4]);
        let mut oem_id = [0u8; 6];
        oem_id.copy_from_slice(&data[10..16]);
        let mut oem_table_id = [0u8; 8];
        oem_table_id.copy_from_slice(&data[16..24]);
        Some(Self {
            signature: SdtSignature(sig),
            length: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            revision: data[8],
            checksum: data[9],
            oem_id,
            oem_table_id,
            oem_revision: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            creator_id: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            creator_revision: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
        })
    }

    pub fn validate_checksum(&self, data: &[u8]) -> bool {
        if data.len() < self.length as usize { return false; }
        let sum: u8 = data[..self.length as usize].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        sum == 0
    }
}
