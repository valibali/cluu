//! Boot manifest schema and parser.
//!
//! This is the policy description consumed from initrd during early userspace
//! bootstrap. M0 keeps this parser permissive-at-presence (optional file) and
//! strict-on-parse (malformed manifest is rejected).

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{Error, Result};

/// Default path of the boot manifest inside initrd tar.
pub const DEFAULT_BOOT_MANIFEST_PATH: &str = "sys/boot.manifest";

/// Current manifest format version.
pub const BOOT_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootManifest {
    pub version: u32,
    pub signature_hex: String,
    pub services: Vec<ServiceManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManifestEntry {
    pub path: String,
    /// Lowercase hex SHA-256 digest (64 chars).
    pub sha256_hex: String,
    /// Declared rights mask for initial token shaping.
    pub rights_mask: u32,
}

/// Parse UTF-8 text manifest.
///
/// Format:
/// - `manifest_version=1`
/// - `signature=<64hex>`
/// - `service path=<path> sha256=<64hex> rights=0x<hex>`
pub fn parse_boot_manifest(data: &[u8]) -> Result<BootManifest> {
    let text = core::str::from_utf8(data).map_err(|_| Error::InvalidArgument)?;

    let mut version: Option<u32> = None;
    let mut signature_hex: Option<String> = None;
    let mut services = Vec::new();
    let mut seen_paths = BTreeSet::new();

    for (line_no, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(value) = line.strip_prefix("manifest_version=") {
            if version.is_some() {
                return Err(Error::AlreadyExists);
            }
            let parsed = value.parse::<u32>().map_err(|_| Error::InvalidArgument)?;
            if parsed != BOOT_MANIFEST_VERSION {
                return Err(Error::InvalidArgument);
            }
            version = Some(parsed);
            continue;
        }

        if let Some(value) = line.strip_prefix("signature=") {
            if signature_hex.is_some() {
                return Err(Error::AlreadyExists);
            }
            if !is_lower_hex_64(value) {
                return Err(Error::InvalidArgument);
            }
            signature_hex = Some(value.to_string());
            continue;
        }

        let Some(rest) = line.strip_prefix("service ") else {
            let _ = line_no;
            return Err(Error::InvalidArgument);
        };

        let entry = parse_service_entry(rest)?;
        if !seen_paths.insert(entry.path.clone()) {
            return Err(Error::AlreadyExists);
        }
        services.push(entry);
    }

    if version != Some(BOOT_MANIFEST_VERSION) || services.is_empty() {
        return Err(Error::InvalidArgument);
    }

    Ok(BootManifest {
        version: BOOT_MANIFEST_VERSION,
        signature_hex: signature_hex.ok_or(Error::InvalidArgument)?,
        services,
    })
}

fn parse_service_entry(rest: &str) -> Result<ServiceManifestEntry> {
    let mut path: Option<String> = None;
    let mut sha256_hex: Option<String> = None;
    let mut rights_mask: Option<u32> = None;

    for token in rest.split_whitespace() {
        let (key, value) = token.split_once('=').ok_or(Error::InvalidArgument)?;
        match key {
            "path" => {
                if value.is_empty() {
                    return Err(Error::InvalidArgument);
                }
                path = Some(value.to_string());
            }
            "sha256" => {
                if !is_lower_hex_64(value) {
                    return Err(Error::InvalidArgument);
                }
                sha256_hex = Some(value.to_string());
            }
            "rights" => {
                rights_mask = Some(parse_hex_u32(value)?);
            }
            _ => return Err(Error::InvalidArgument),
        }
    }

    let path = path.ok_or(Error::InvalidArgument)?;
    let sha256_hex = sha256_hex.ok_or(Error::InvalidArgument)?;
    let rights_mask = rights_mask.ok_or(Error::InvalidArgument)?;

    Ok(ServiceManifestEntry {
        path,
        sha256_hex,
        rights_mask,
    })
}

fn is_lower_hex_64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn parse_hex_u32(s: &str) -> Result<u32> {
    let hex = s.strip_prefix("0x").ok_or(Error::InvalidArgument)?;
    u32::from_str_radix(hex, 16).map_err(|_| Error::InvalidArgument)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_manifest() {
        let manifest = parse_boot_manifest(
            br#"
manifest_version=1
signature=1111111111111111111111111111111111111111111111111111111111111111
service path=sys/registry sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa rights=0x0000000f
service path=sys/procmgr sha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb rights=0x0000ffff
"#,
        )
        .expect("manifest should parse");

        assert_eq!(manifest.version, 1);
        assert_eq!(
            manifest.signature_hex,
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        assert_eq!(manifest.services.len(), 2);
        assert_eq!(manifest.services[0].path, "sys/registry");
        assert_eq!(manifest.services[1].rights_mask, 0x0000ffff);
    }

    #[test]
    fn reject_missing_version() {
        let err = parse_boot_manifest(
            br#"
service path=sys/registry sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa rights=0x1
"#,
        )
        .expect_err("missing version must fail");
        assert_eq!(err, Error::InvalidArgument);
    }

    #[test]
    fn reject_duplicate_service_path() {
        let err = parse_boot_manifest(
            br#"
manifest_version=1
signature=1111111111111111111111111111111111111111111111111111111111111111
service path=sys/registry sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa rights=0x1
service path=sys/registry sha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb rights=0x2
"#,
        )
        .expect_err("duplicate paths must fail");
        assert_eq!(err, Error::AlreadyExists);
    }

    #[test]
    fn reject_invalid_sha_format() {
        let err = parse_boot_manifest(
            br#"
manifest_version=1
signature=1111111111111111111111111111111111111111111111111111111111111111
service path=sys/registry sha256=ABCD rights=0x1
"#,
        )
        .expect_err("uppercase/short sha must fail");
        assert_eq!(err, Error::InvalidArgument);
    }

    #[test]
    fn reject_missing_signature() {
        let err = parse_boot_manifest(
            br#"
manifest_version=1
service path=sys/registry sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa rights=0x1
"#,
        )
        .expect_err("missing signature must fail");
        assert_eq!(err, Error::InvalidArgument);
    }
}
