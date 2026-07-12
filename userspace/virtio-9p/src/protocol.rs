//! 9P2000.L protocol types and encoding/decoding helpers.

extern crate alloc;

pub const RLERROR: u8 = 7;
pub const TLOPEN: u8 = 12;
pub const RLOPEN: u8 = 13;
pub const TGETATTR: u8 = 24;
pub const RGETATTR: u8 = 25;
pub const TREADDIR: u8 = 40;
pub const RREADDIR: u8 = 41;
pub const TMKDIR: u8 = 72;
pub const RMKDIR: u8 = 73;
pub const TUNLINKAT: u8 = 76;
pub const RUNLINKAT: u8 = 77;
pub const TVERSION: u8 = 100;
pub const RVERSION: u8 = 101;
pub const TATTACH: u8 = 104;
pub const RATTACH: u8 = 105;
pub const TWALK: u8 = 110;
pub const RWALK: u8 = 111;
pub const TREAD: u8 = 116;
pub const RREAD: u8 = 117;
pub const TWRITE: u8 = 118;
pub const RWRITE: u8 = 119;
pub const TCLUNK: u8 = 120;
pub const RCLUNK: u8 = 121;

pub const QID_SIZE: usize = 13;
pub const QTDIR: u8 = 0x80;

pub const GETATTR_MODE: u64 = 0x01;
pub const GETATTR_NLINK: u64 = 0x02;
pub const GETATTR_UID: u64 = 0x04;
pub const GETATTR_GID: u64 = 0x08;
pub const GETATTR_SIZE: u64 = 0x200;
pub const GETATTR_MTIME: u64 = 0x40;
pub const GETATTR_ALL: u64 = 0x3FFF;

pub struct Qid {
    pub type_: u8,
    pub version: u32,
    pub path: u64,
}

#[derive(Clone)]
pub struct GetAttr {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub size: u64,
    pub mtime: u64,
}

pub struct Encoder<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Encoder<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn bytes_written(&self) -> usize {
        self.pos
    }

    pub fn put_u8(&mut self, v: u8) -> bool {
        if self.pos + 1 > self.buf.len() {
            return false;
        }
        self.buf[self.pos] = v;
        self.pos += 1;
        true
    }

    pub fn put_u16(&mut self, v: u16) -> bool {
        if self.pos + 2 > self.buf.len() {
            return false;
        }
        self.buf[self.pos..self.pos + 2].copy_from_slice(&v.to_le_bytes());
        self.pos += 2;
        true
    }

    pub fn put_u32(&mut self, v: u32) -> bool {
        if self.pos + 4 > self.buf.len() {
            return false;
        }
        self.buf[self.pos..self.pos + 4].copy_from_slice(&v.to_le_bytes());
        self.pos += 4;
        true
    }

    pub fn put_u64(&mut self, v: u64) -> bool {
        if self.pos + 8 > self.buf.len() {
            return false;
        }
        self.buf[self.pos..self.pos + 8].copy_from_slice(&v.to_le_bytes());
        self.pos += 8;
        true
    }

    pub fn put_string(&mut self, s: &str) -> bool {
        let bytes = s.as_bytes();
        if bytes.len() > 0xFFFF {
            return false;
        }
        if !self.put_u16(bytes.len() as u16) {
            return false;
        }
        if self.pos + bytes.len() > self.buf.len() {
            return false;
        }
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
        true
    }

    pub fn put_bytes(&mut self, b: &[u8]) -> bool {
        if self.pos + b.len() > self.buf.len() {
            return false;
        }
        self.buf[self.pos..self.pos + b.len()].copy_from_slice(b);
        self.pos += b.len();
        true
    }
}

pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn get_u8(&mut self) -> Option<u8> {
        if self.pos + 1 > self.buf.len() {
            return None;
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Some(v)
    }

    pub fn get_u16(&mut self) -> Option<u16> {
        if self.pos + 2 > self.buf.len() {
            return None;
        }
        let mut bytes = [0u8; 2];
        bytes.copy_from_slice(&self.buf[self.pos..self.pos + 2]);
        let v = u16::from_le_bytes(bytes);
        self.pos += 2;
        Some(v)
    }

    pub fn get_u32(&mut self) -> Option<u32> {
        if self.pos + 4 > self.buf.len() {
            return None;
        }
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        let v = u32::from_le_bytes(bytes);
        self.pos += 4;
        Some(v)
    }

    pub fn get_u64(&mut self) -> Option<u64> {
        if self.pos + 8 > self.buf.len() {
            return None;
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        let v = u64::from_le_bytes(bytes);
        self.pos += 8;
        Some(v)
    }

    pub fn get_string(&mut self) -> Option<&'a str> {
        let len = self.get_u16()? as usize;
        if self.pos + len > self.buf.len() {
            return None;
        }
        let s = core::str::from_utf8(&self.buf[self.pos..self.pos + len]).ok()?;
        self.pos += len;
        Some(s)
    }

    pub fn get_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.pos + len > self.buf.len() {
            return None;
        }
        let b = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Some(b)
    }

    pub fn get_qid(&mut self) -> Option<Qid> {
        let type_ = self.get_u8()?;
        let version = self.get_u32()?;
        let path = self.get_u64()?;
        Some(Qid { type_, version, path })
    }
}
