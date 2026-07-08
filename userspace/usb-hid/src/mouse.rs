use crate::hid;

#[derive(Debug, Clone, Copy, Default)]
pub struct MouseReport {
    pub buttons: u8,
    pub dx: i8,
    pub dy: i8,
}

impl MouseReport {
    pub fn from_bytes(data: &[u8; 4]) -> Self {
        Self {
            buttons: data[0],
            dx: data[1] as i8,
            dy: data[2] as i8,
        }
    }

    pub fn left_button(&self) -> bool { self.buttons & 1 != 0 }
    pub fn right_button(&self) -> bool { self.buttons & 2 != 0 }
    pub fn middle_button(&self) -> bool { self.buttons & 4 != 0 }
}

pub struct HidMouse {
    pub slot_id: u8,
    pub addr: u8,
    pub ep_in: u8,
    pub last_report: MouseReport,
}

impl HidMouse {
    pub fn new(slot_id: u8, addr: u8, ep_in: u8) -> Self {
        Self { slot_id, addr, ep_in, last_report: MouseReport::default() }
    }

    pub fn set_boot_protocol(&self) {
        let _ = hid::USB_REQ_SET_PROTOCOL;
    }

    pub fn poll(&mut self, data: &[u8]) -> Option<MouseReport> {
        if let Some(raw) = hid::parse_boot_mouse(data) {
            let report = MouseReport::from_bytes(&raw);
            let changed = report.buttons != self.last_report.buttons
                || report.dx != self.last_report.dx
                || report.dy != self.last_report.dy;
            self.last_report = report;
            if changed { Some(report) } else { None }
        } else {
            None
        }
    }
}
