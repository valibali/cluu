//! PID layout: 8-bit session id (high) | 23-bit local pid (low).
//! `pid_t` is `i32`; sign bit reserved; 31 usable bits.

pub type SessionId = u8;
pub type LocalPid = u32; // 23-bit effective
pub type Pid = i32;

pub const SID_BITS: u32 = 8;
pub const LOCAL_BITS: u32 = 23;
pub const LOCAL_MAX: u32 = (1u32 << LOCAL_BITS) - 1;

#[derive(Debug, PartialEq, Eq)]
pub enum PidError {
    LocalOutOfRange,
}

pub fn encode(sid: SessionId, local: LocalPid) -> Result<Pid, PidError> {
    if local > LOCAL_MAX {
        return Err(PidError::LocalOutOfRange);
    }
    Ok(((sid as i32) << LOCAL_BITS) | (local as i32))
}

pub fn decode(pid: Pid) -> (SessionId, LocalPid) {
    let local = (pid as u32) & LOCAL_MAX;
    let sid = ((pid as u32) >> LOCAL_BITS) as u8;
    (sid, local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn encode_decode_roundtrip_smoke() {
        assert_eq!(encode(0, 0).unwrap(), 0);
        assert_eq!(encode(5, 1).unwrap(), 0x2800001);
        assert_eq!(encode(255, LOCAL_MAX).unwrap(), 0x7FFFFFFF);
        assert_eq!(decode(0x2800001), (5, 1));
    }

    #[test]
    fn encode_local_overflow_errors() {
        assert_eq!(encode(0, LOCAL_MAX + 1), Err(PidError::LocalOutOfRange));
    }

    proptest! {
        #[test]
        fn prop_encode_decode_roundtrip(sid in 0u8..=255, local in 0u32..=LOCAL_MAX) {
            let pid = encode(sid, local).unwrap();
            let (s, l) = decode(pid);
            prop_assert_eq!(s, sid);
            prop_assert_eq!(l, local);
            prop_assert!(pid >= 0); // never negative (sign bit clear)
        }
    }
}
