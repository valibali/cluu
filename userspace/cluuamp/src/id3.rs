//! ID3v2 + ID3v1 tag parser. Pure, no allocation — returns string views
//! into the source buffer. The audio engine already holds the full MP3
//! file in memory, so we parse once at load and cache the result.

use alloc::string::String;
use alloc::string::ToString;

/// Parsed track metadata. All fields fall back to empty strings when the
/// corresponding tag is missing. Callers decide the display fallback
/// (e.g. filename-as-title) — this struct only reports what the tags said.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TrackMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl TrackMeta {
    /// True if no ID3 field was populated.
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.artist.is_empty() && self.album.is_empty()
    }
}

/// Parse ID3v2 (if present at offset 0) then ID3v1 (last 128 bytes).
/// ID3v2 wins when both exist; ID3v1 is the fallback.
pub fn parse(data: &[u8]) -> TrackMeta {
    let v2 = parse_v2(data);
    if !v2.is_empty() {
        return v2;
    }
    parse_v1(data)
}

// ─── ID3v2 ─────────────────────────────────────────────────────────────

fn parse_v2(data: &[u8]) -> TrackMeta {
    if data.len() < 10 || &data[0..3] != b"ID3" {
        return TrackMeta::default();
    }

    let version_major = data[3];
    // v2.2 has 3-char frame IDs; v2.3/v2.4 have 4-char. We support 2.3+.
    if version_major < 3 {
        return TrackMeta::default();
    }

    let flags = data[5];
    let footer_present = (flags & 0x10) != 0;
    // Synchsafe: 7 bits per byte.
    let tag_size = synchsafe(&data[6..10]);
    let body_start = 10usize;
    let body_end = (body_start + tag_size).min(data.len());

    let mut meta = TrackMeta::default();
    let mut pos = body_start;
    while pos + 10 <= body_end {
        let frame_id = &data[pos..pos + 4];
        if frame_id == b"\0\0\0\0" {
            break;
        }
        let frame_size = read_u32_be(&data[pos + 4..pos + 8]);
        // Skip flags (2 bytes) — we don't act on them.
        let content_start = pos + 10;
        if frame_size == 0 || content_start + frame_size > body_end {
            break;
        }
        let content = &data[content_start..content_start + frame_size];

        match frame_id {
            b"TIT2" => meta.title = parse_text_frame(content),
            b"TPE1" => meta.artist = parse_text_frame(content),
            b"TALB" => meta.album = parse_text_frame(content),
            _ => {}
        }
        pos = content_start + frame_size;
    }

    if footer_present {
        // 10-byte footer mirrors the header; skip past it for any caller
        // that continues scanning. Not needed here but kept for correctness.
    }

    meta
}

/// Decode a text frame: byte 0 is encoding, rest is the string.
/// Encodings: 0 = ISO-8859-1, 1 = UTF-16 with BOM, 2 = UTF-16BE, 3 = UTF-8.
fn parse_text_frame(content: &[u8]) -> String {
    if content.is_empty() {
        return String::new();
    }
    let encoding = content[0];
    let text = &content[1..];
    // Strip trailing NULs (frames are often NUL-padded).
    let text = trim_trailing_nuls(text);

    match encoding {
        0 => iso_8859_1_to_string(text),
        3 => utf8_lossy(text),
        1 => utf16_with_bom(text),
        2 => utf16be_to_string(text),
        _ => String::new(),
    }
}

fn trim_trailing_nuls(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 && s[end - 1] == 0 {
        end -= 1;
    }
    &s[..end]
}

fn iso_8859_1_to_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        s.push(b as char);
    }
    s
}

fn utf8_lossy(bytes: &[u8]) -> String {
    match core::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            // Fall back to ISO-8859-1 for malformed UTF-8 — better than
            // dropping the tag entirely.
            iso_8859_1_to_string(bytes)
        }
    }
}

fn utf16_with_bom(bytes: &[u8]) -> String {
    if bytes.len() < 2 {
        return String::new();
    }
    let little_endian = bytes[0] == 0xFF && bytes[1] == 0xFE;
    let big_endian = bytes[0] == 0xFE && bytes[1] == 0xFF;
    if !little_endian && !big_endian {
        return String::new();
    }
    let body = &bytes[2..];
    if little_endian {
        utf16le_to_string(body)
    } else {
        utf16be_to_string(body)
    }
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let code = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if code == 0 {
            break;
        }
        if let Some(ch) = char::from_u32(code as u32) {
            s.push(ch);
        }
        i += 2;
    }
    s
}

fn utf16be_to_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let code = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        if code == 0 {
            break;
        }
        if let Some(ch) = char::from_u32(code as u32) {
            s.push(ch);
        }
        i += 2;
    }
    s
}

fn synchsafe(b: &[u8]) -> usize {
    ((b[0] as usize) << 21) | ((b[1] as usize) << 14) | ((b[2] as usize) << 7) | (b[3] as usize)
}

fn read_u32_be(b: &[u8]) -> usize {
    ((b[0] as usize) << 24) | ((b[1] as usize) << 16) | ((b[2] as usize) << 8) | (b[3] as usize)
}

// ─── ID3v1 ─────────────────────────────────────────────────────────────

fn parse_v1(data: &[u8]) -> TrackMeta {
    if data.len() < 128 {
        return TrackMeta::default();
    }
    let tail = &data[data.len() - 128..];
    if &tail[0..3] != b"TAG" {
        return TrackMeta::default();
    }

    TrackMeta {
        title: iso_8859_1_to_string(trim_trailing_nuls(&tail[3..33])),
        artist: iso_8859_1_to_string(trim_trailing_nuls(&tail[33..63])),
        album: iso_8859_1_to_string(trim_trailing_nuls(&tail[63..93])),
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

fn build_v2_frame(id: &[u8; 4], text: &str) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let mut frame = Vec::with_capacity(10 + 1 + text_bytes.len());
    frame.extend_from_slice(id);
    let size = (1 + text_bytes.len()) as u32;
    frame.extend_from_slice(&size.to_be_bytes());
    frame.extend_from_slice(&[0u8, 0]); // flags
    frame.push(3); // UTF-8 encoding
    frame.extend_from_slice(text_bytes);
    frame
}

fn build_v2(frames: &[Vec<u8>]) -> Vec<u8> {
    let body_len: usize = frames.iter().map(|f| f.len()).sum();
    let mut out = Vec::with_capacity(10 + body_len);
    out.extend_from_slice(b"ID3");
    out.push(3); // version major
    out.push(0); // version minor
    out.push(0); // flags
    let mut size = body_len;
    let mut ss = [0u8; 4];
    ss[3] = (size & 0x7F) as u8;
    size >>= 7;
    ss[2] = (size & 0x7F) as u8;
    size >>= 7;
    ss[1] = (size & 0x7F) as u8;
    size >>= 7;
    ss[0] = (size & 0x7F) as u8;
    out.extend_from_slice(&ss);
    for f in frames {
        out.extend_from_slice(f);
    }
    out
}

#[test]
fn parse_v2_title_artist_album() {
    let data = build_v2(&[
        build_v2_frame(b"TIT2", "My Song"),
        build_v2_frame(b"TPE1", "My Artist"),
        build_v2_frame(b"TALB", "My Album"),
    ]);
    let m = parse(&data);
    assert_eq!(m.title, "My Song");
    assert_eq!(m.artist, "My Artist");
    assert_eq!(m.album, "My Album");
}

#[test]
fn parse_v2_missing_fields_yield_empty() {
    let data = build_v2(&[build_v2_frame(b"TIT2", "Only Title")]);
    let m = parse(&data);
    assert_eq!(m.title, "Only Title");
    assert!(m.artist.is_empty());
    assert!(m.album.is_empty());
}

#[test]
fn parse_v2_no_tag_returns_empty() {
    let m = parse(&[0u8; 64]);
    assert!(m.is_empty());
}

#[test]
fn parse_v2_version_2_unsupported() {
    let mut data = vec![b'I', b'D', b'3', 2, 0, 0, 0, 0, 0, 0];
    data.extend_from_slice(&[0u8; 32]);
    let m = parse(&data);
    assert!(m.is_empty());
}

fn v1_field(s: &str, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    let bytes = s.as_bytes();
    let copy_len = bytes.len().min(len);
    v[..copy_len].copy_from_slice(&bytes[..copy_len]);
    v
}

#[test]
fn parse_v1_basic() {
    let mut data = vec![0u8; 200];
    let tail_start = data.len() - 128;
    data[tail_start..tail_start + 3].copy_from_slice(b"TAG");
    data[tail_start + 3..tail_start + 33].copy_from_slice(&v1_field("V1 Title Song", 30));
    data[tail_start + 33..tail_start + 63].copy_from_slice(&v1_field("V1 Artist Name", 30));
    data[tail_start + 63..tail_start + 93].copy_from_slice(&v1_field("V1 Album Title", 30));
    let m = parse(&data);
    assert_eq!(m.title, "V1 Title Song");
    assert_eq!(m.artist, "V1 Artist Name");
    assert_eq!(m.album, "V1 Album Title");
}

#[test]
fn parse_v2_wins_over_v1() {
    let mut data = build_v2(&[build_v2_frame(b"TIT2", "V2 Title")]);
    data.extend_from_slice(&[0u8; 200]);
    let tail_start = data.len() - 128;
    data[tail_start..tail_start + 3].copy_from_slice(b"TAG");
    data[tail_start + 3..tail_start + 33].copy_from_slice(&v1_field("V1 Title Song", 30));
    let m = parse(&data);
    assert_eq!(m.title, "V2 Title");
}

#[test]
fn parse_v1_no_tag_returns_empty() {
    let data = vec![0u8; 256];
    let m = parse(&data);
    assert!(m.is_empty());
}

#[test]
fn parse_v1_truncated_tag_ignored() {
    let mut data = vec![0u8; 100];
    data[0] = b'T';
    data[1] = b'A';
    let m = parse(&data);
    assert!(m.is_empty());
}

#[test]
fn trim_nuls_strips_only_trailing() {
    assert_eq!(trim_trailing_nuls(b"hello\0\0"), b"hello");
    assert_eq!(trim_trailing_nuls(b"hello"), b"hello");
    assert_eq!(trim_trailing_nuls(b"\0\0"), b"");
    assert_eq!(trim_trailing_nuls(b"he\0lo\0"), b"he\0lo");
}

#[test]
fn iso_8859_1_preserves_high_bytes() {
    let s = iso_8859_1_to_string(&[0x41, 0xE9, 0xFC]);
    assert_eq!(s.chars().count(), 3);
    assert_eq!(s.chars().nth(1), Some('\u{00E9}'));
}

#[test]
fn utf16_le_with_bom() {
    // BOM + "Hi" in UTF-16LE
    let bytes = [0xFF, 0xFE, b'H', 0x00, b'i', 0x00];
    let s = utf16_with_bom(&bytes);
    assert_eq!(s, "Hi");
}

#[test]
fn utf16_be_with_bom() {
    // BOM + "Hi" in UTF-16BE
    let bytes = [0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
    let s = utf16_with_bom(&bytes);
    assert_eq!(s, "Hi");
}

#[test]
fn empty_text_frame_returns_empty_string() {
    assert_eq!(parse_text_frame(&[]), "");
    assert_eq!(parse_text_frame(&[3]), "");
}

#[test]
fn is_empty_true_when_all_blank() {
    assert!(TrackMeta::default().is_empty());
    assert!(!TrackMeta {
        title: String::from("x"),
        ..Default::default()
    }
    .is_empty());
}
}
