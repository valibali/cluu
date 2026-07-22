//! Minimal no_std TOML parser for container manifests.
//!
//! Supports:
//! - `[table]` headers
//! - `key = "string value"` pairs
//! - `key = ["array", "of", "strings"]`
//! - `[[array.of.tables]]` for repeated table entries (e.g. `[[env]]`)
//!
//! Does NOT support: nested inline tables, integers, booleans, dates,
//! multiline strings, or escaped characters beyond `\"` and `\\`.

use alloc::string::String;
use alloc::vec::Vec;

/// A parsed TOML value.
#[derive(Debug, Clone, PartialEq)]
pub enum TomlValue {
    String(String),
    Array(Vec<String>),
}

/// A single key-value entry within a table.
#[derive(Debug, Clone)]
pub struct TomlEntry {
    pub key: String,
    pub value: TomlValue,
}

/// A parsed table — either a `[table]` or one instance of `[[array.of.tables]]`.
#[derive(Debug, Clone)]
pub struct TomlTable {
    pub name: String,
    pub is_array: bool,
    pub entries: Vec<TomlEntry>,
}

/// Parsed TOML document: a sequence of tables.
/// Top-level entries (before any `[table]`) use `name = ""`.
#[derive(Debug, Clone)]
pub struct TomlDoc {
    pub tables: Vec<TomlTable>,
}

impl TomlDoc {
    /// Get the first table with the given name (non-array).
    pub fn table(&self, name: &str) -> Option<&TomlTable> {
        self.tables.iter().find(|t| t.name == name && !t.is_array)
    }

    /// Get all array-of-tables entries with the given name.
    pub fn array_tables(&self, name: &str) -> Vec<&TomlTable> {
        self.tables
            .iter()
            .filter(|t| t.name == name && t.is_array)
            .collect()
    }
}

impl TomlTable {
    /// Get a string value by key.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.entries.iter().find_map(|e| {
            if e.key == key {
                if let TomlValue::String(ref s) = e.value {
                    return Some(s.as_str());
                }
            }
            None
        })
    }

    /// Get a string array value by key.
    pub fn get_array(&self, key: &str) -> Option<&[String]> {
        self.entries.iter().find_map(|e| {
            if e.key == key {
                if let TomlValue::Array(ref a) = e.value {
                    return Some(a.as_slice());
                }
            }
            None
        })
    }
}

/// Parse a TOML string into a `TomlDoc`.
pub fn parse(input: &str) -> Result<TomlDoc, ParseError> {
    let mut tables = Vec::new();
    // Implicit top-level table for entries before any [header]
    let mut current = TomlTable {
        name: String::new(),
        is_array: false,
        entries: Vec::new(),
    };

    let mut iter = input.lines().enumerate();
    while let Some((line_idx, raw_line)) = iter.next() {
        let lineno = line_idx + 1;
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with("[[") {
            // Array-of-tables header: [[name]]
            let end = line
                .find("]]")
                .ok_or(ParseError { line: lineno, msg: "unterminated [[" })?;
            let name = line[2..end].trim();
            if name.is_empty() {
                return Err(ParseError { line: lineno, msg: "empty [[]] table name" });
            }
            tables.push(current);
            current = TomlTable {
                name: String::from(name),
                is_array: true,
                entries: Vec::new(),
            };
        } else if line.starts_with('[') {
            // Regular table header: [name]
            let end = line
                .find(']')
                .ok_or(ParseError { line: lineno, msg: "unterminated [" })?;
            let name = line[1..end].trim();
            if name.is_empty() {
                return Err(ParseError { line: lineno, msg: "empty [] table name" });
            }
            tables.push(current);
            current = TomlTable {
                name: String::from(name),
                is_array: false,
                entries: Vec::new(),
            };
        } else {
            // Key-value pair: key = value
            let eq_pos = line
                .find('=')
                .ok_or(ParseError { line: lineno, msg: "expected key = value" })?;
            let key = line[..eq_pos].trim();
            let val_str = line[eq_pos + 1..].trim();

            if key.is_empty() {
                return Err(ParseError { line: lineno, msg: "empty key" });
            }

            let value = if val_str.starts_with('[') {
                // Array of strings: ["a", "b", "c"] — single- or multi-line.
                if val_str.contains(']') {
                    parse_string_array(val_str, lineno)?
                } else {
                    // Multi-line array: accumulate continuation lines until ']'.
                    let mut accum = String::from(val_str);
                    let mut closed = false;
                    while let Some((_, cont_raw)) = iter.next() {
                        let cont = cont_raw.trim();
                        if cont.is_empty() || cont.starts_with('#') {
                            continue;
                        }
                        accum.push(' ');
                        accum.push_str(cont);
                        if cont.contains(']') {
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err(ParseError { line: lineno, msg: "unterminated array" });
                    }
                    parse_string_array(&accum, lineno)?
                }
            } else if val_str.starts_with('"') {
                // Quoted string
                TomlValue::String(parse_quoted_string(val_str, lineno)?)
            } else {
                // Bare value — treat as unquoted string
                TomlValue::String(String::from(val_str))
            };

            current.entries.push(TomlEntry {
                key: String::from(key),
                value,
            });
        }
    }

    tables.push(current);
    Ok(TomlDoc { tables })
}

/// Parse error with line number.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub line: usize,
    pub msg: &'static str,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TOML parse error at line {}: {}", self.line, self.msg)
    }
}

fn parse_quoted_string(s: &str, lineno: usize) -> Result<String, ParseError> {
    if !s.starts_with('"') {
        return Err(ParseError { line: lineno, msg: "expected opening quote" });
    }
    let mut out = String::new();
    let mut chars = s[1..].chars();
    loop {
        match chars.next() {
            None => return Err(ParseError { line: lineno, msg: "unterminated string" }),
            Some('"') => return Ok(out),
            Some('\\') => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                _ => return Err(ParseError { line: lineno, msg: "unknown escape sequence" }),
            },
            Some(c) => out.push(c),
        }
    }
}

fn parse_string_array(s: &str, lineno: usize) -> Result<TomlValue, ParseError> {
    if !s.starts_with('[') {
        return Err(ParseError { line: lineno, msg: "expected [" });
    }
    let end = s.rfind(']').ok_or(ParseError { line: lineno, msg: "unterminated array" })?;
    let inner = s[1..end].trim();
    if inner.is_empty() {
        return Ok(TomlValue::Array(Vec::new()));
    }

    let mut items = Vec::new();
    let mut rest = inner;
    while !rest.is_empty() {
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.is_empty() {
            break;
        }
        if rest_trimmed.starts_with('"') {
            let val = parse_quoted_string(rest_trimmed, lineno)?;
            items.push(val);
            // Skip past the closing quote and optional comma
            let after_quote = &rest_trimmed[1..]; // skip opening quote
            let mut skip = 0;
            let mut in_escape = false;
            for ch in after_quote.chars() {
                skip += ch.len_utf8();
                if in_escape {
                    in_escape = false;
                    continue;
                }
                if ch == '\\' {
                    in_escape = true;
                    continue;
                }
                if ch == '"' {
                    break;
                }
            }
            let remainder = &after_quote[skip..];
            rest = remainder.trim_start();
            if rest.starts_with(',') {
                rest = rest[1..].trim_start();
            }
        } else {
            // Bare value (integer, hex, boolean) — treat as unquoted string
            // up to the next comma or closing bracket. Consistent with the
            // bare-value handling for scalar key=value pairs above.
            let end = rest_trimmed
                .find(|c: char| c == ',' || c == ']')
                .unwrap_or(rest_trimmed.len());
            let val = rest_trimmed[..end].trim();
            if !val.is_empty() {
                items.push(String::from(val));
            }
            let remainder = &rest_trimmed[end..];
            rest = remainder.trim_start();
            if rest.starts_with(',') {
                rest = rest[1..].trim_start();
            }
        }
    }

    Ok(TomlValue::Array(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_parse() {
        let input = r#"
[container]
base = "minimal"
entrypoint = "/bin/hello"

[profile]
caps = ["ipc", "vfs"]

[[env]]
key = "HOME"
value = "/home/user"

[[env]]
key = "PATH"
value = "/bin"
"#;
        let doc = parse(input).unwrap();
        let container = doc.table("container").unwrap();
        assert_eq!(container.get_str("base"), Some("minimal"));
        assert_eq!(container.get_str("entrypoint"), Some("/bin/hello"));

        let profile = doc.table("profile").unwrap();
        let caps = profile.get_array("caps").unwrap();
        assert_eq!(caps, &["ipc", "vfs"]);

        let envs = doc.array_tables("env");
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].get_str("key"), Some("HOME"));
        assert_eq!(envs[0].get_str("value"), Some("/home/user"));
        assert_eq!(envs[1].get_str("key"), Some("PATH"));
        assert_eq!(envs[1].get_str("value"), Some("/bin"));
    }

    #[test]
    fn test_empty_array() {
        let input = r#"
[section]
items = []
"#;
        let doc = parse(input).unwrap();
        let section = doc.table("section").unwrap();
        assert_eq!(section.get_array("items"), Some([].as_slice()));
    }

    #[test]
    fn test_escape_sequences() {
        let input = r#"
[test]
path = "C:\\Users\\test"
greeting = "hello \"world\""
"#;
        let doc = parse(input).unwrap();
        let test = doc.table("test").unwrap();
        assert_eq!(test.get_str("path"), Some("C:\\Users\\test"));
        assert_eq!(test.get_str("greeting"), Some("hello \"world\""));
    }

    #[test]
    fn test_top_level_entries() {
        let input = r#"
name = "myapp"
version = "1.0"
"#;
        let doc = parse(input).unwrap();
        let top = doc.table("").unwrap();
        assert_eq!(top.get_str("name"), Some("myapp"));
        assert_eq!(top.get_str("version"), Some("1.0"));
    }

    #[test]
    fn test_error_unterminated_string() {
        let input = r#"
[section]
key = "unterminated
"#;
        assert!(parse(input).is_err());
    }

    #[test]
    fn test_error_missing_equals() {
        let input = r#"
[section]
no_equals_here
"#;
        assert!(parse(input).is_err());
    }

    #[test]
    fn parses_single_line_array() {
        let input = r#"[t]
xs = ["a", "b", "c"]
"#;
        let doc = parse(input).expect("parse");
        let table = doc.table("t").expect("t");
        let arr = table.get_array("xs").expect("xs");
        assert_eq!(arr, &["a", "b", "c"]);
    }

    #[test]
    fn parses_multi_line_array() {
        let input = r#"[t]
xs = [
    "a",
    "b",
    "c",
]
"#;
        let doc = parse(input).expect("parse");
        let table = doc.table("t").expect("t");
        let arr = table.get_array("xs").expect("xs");
        assert_eq!(arr, &["a", "b", "c"]);
    }

    #[test]
    fn parses_envelope_service_block() {
        // Verbatim copy of [envelope.service] block from /etc/envelopes.toml.
        let input = r#"[envelope.service]
# Stripped envelope for daemons spawned by procmgr/init at boot.
# No /home, no /tmp by default — services declare what they need
# in their own Cluufiles (which then narrow within this envelope).
mounts = [
    "ro:/",
    "ro:/etc",
    "ro:/lib",
    "rw:/var/log",
]

[envelope.service.env]
PATH = "/sbin:/bin"
TERM = "dumb"
LANG = "C"
"#;
        let doc = parse(input).expect("parse");
        let svc = doc.table("envelope.service").expect("envelope.service");
        let mounts = svc.get_array("mounts").expect("mounts");
        assert_eq!(mounts.len(), 4);
        assert_eq!(mounts[0], "ro:/");
        assert_eq!(mounts[3], "rw:/var/log");

        let env = doc.table("envelope.service.env").expect("env");
        assert_eq!(env.get_str("PATH"), Some("/sbin:/bin"));
    }

    #[test]
    fn unterminated_multi_line_array_errors() {
        let input = "[t]\nxs = [\n  \"a\",\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn parses_multi_line_array_with_comments_and_blanks() {
        let input = r#"[t]
xs = [
    # a comment

    "a",
    "b",
]
"#;
        let doc = parse(input).expect("parse");
        let table = doc.table("t").expect("t");
        let arr = table.get_array("xs").expect("xs");
        assert_eq!(arr, &["a", "b"]);
    }
}
