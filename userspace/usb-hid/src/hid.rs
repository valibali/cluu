pub const HID_CLASS: u8 = 0x03;
pub const HID_BOOT_SUBCLASS: u8 = 0x01;
pub const HID_BOOT_PROTOCOL_KBD: u8 = 0x01;
pub const HID_BOOT_PROTOCOL_MOUSE: u8 = 0x02;

pub const HID_DESCRIPTOR_TYPE: u8 = 0x21;
pub const HID_REPORT_DESCRIPTOR_TYPE: u8 = 0x22;

pub const USB_REQ_GET_DESCRIPTOR: u8 = 0x06;
pub const USB_REQ_SET_PROTOCOL: u8 = 0x0B;
pub const USB_REQ_SET_IDLE: u8 = 0x0A;

pub fn parse_boot_kbd(data: &[u8]) -> Option<[u8; 8]> {
    if data.len() >= 8 {
        let mut report = [0u8; 8];
        report.copy_from_slice(&data[..8]);
        Some(report)
    } else {
        None
    }
}

pub fn parse_boot_mouse(data: &[u8]) -> Option<[u8; 4]> {
    if data.len() >= 4 {
        let mut report = [0u8; 4];
        report.copy_from_slice(&data[..4]);
        Some(report)
    } else {
        None
    }
}
