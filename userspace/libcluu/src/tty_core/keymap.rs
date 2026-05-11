//! Extended-key → xterm CSI byte sequence map.

/// Translate an extended-key code (as emitted by `userspace/kbd`) into the
/// xterm-style CSI byte sequence that should be fed through the line
/// discipline. Returns `None` if the code is not an extended key (caller
/// then falls back to the ASCII byte path).
pub fn encode_extended(extended: u8) -> Option<&'static [u8]> {
    Some(match extended {
        1 => b"\x1b[A",
        2 => b"\x1b[B",
        3 => b"\x1b[D",
        4 => b"\x1b[C",
        5 => b"\x1b[H",
        6 => b"\x1b[F",
        7 => b"\x1b[3~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn key_up()    { assert_eq!(encode_extended(1), Some(b"\x1b[A".as_ref())); }
    #[test] fn key_down()  { assert_eq!(encode_extended(2), Some(b"\x1b[B".as_ref())); }
    #[test] fn key_left()  { assert_eq!(encode_extended(3), Some(b"\x1b[D".as_ref())); }
    #[test] fn key_right() { assert_eq!(encode_extended(4), Some(b"\x1b[C".as_ref())); }
    #[test] fn key_home()  { assert_eq!(encode_extended(5), Some(b"\x1b[H".as_ref())); }
    #[test] fn key_end()   { assert_eq!(encode_extended(6), Some(b"\x1b[F".as_ref())); }
    #[test] fn key_del()   { assert_eq!(encode_extended(7), Some(b"\x1b[3~".as_ref())); }
    #[test] fn key_unknown() { assert_eq!(encode_extended(0), None); }
}
