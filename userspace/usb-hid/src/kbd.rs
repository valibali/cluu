use crate::hid;

#[derive(Debug, Clone, Copy, Default)]
pub struct KbdReport {
    pub modifiers: u8,
    pub keys: [u8; 6],
}

impl KbdReport {
    pub fn from_bytes(data: &[u8; 8]) -> Self {
        Self {
            modifiers: data[0],
            keys: [data[2], data[3], data[4], data[5], data[6], data[7]],
        }
    }

    pub fn key_codes(&self) -> &[u8; 6] {
        &self.keys
    }
}

pub struct HidKeyboard {
    pub slot_id: u8,
    pub addr: u8,
    pub ep_in: u8,
    pub last_report: KbdReport,
}

impl HidKeyboard {
    pub fn new(slot_id: u8, addr: u8, ep_in: u8) -> Self {
        Self { slot_id, addr, ep_in, last_report: KbdReport::default() }
    }

    pub fn set_boot_protocol(&self) {
        let _ = hid::USB_REQ_SET_PROTOCOL;
    }

    pub fn poll(&mut self, data: &[u8]) -> Option<KbdReport> {
        if let Some(raw) = hid::parse_boot_kbd(data) {
            let report = KbdReport::from_bytes(&raw);
            let changed = report.modifiers != self.last_report.modifiers
                || report.keys != self.last_report.keys;
            self.last_report = report;
            if changed { Some(report) } else { None }
        } else {
            None
        }
    }
}
