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

    for (line_idx, raw_line) in input.lines().enumerate() {
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
                // Array of strings: ["a", "b", "c"]
                parse_string_array(val_str, lineno)?
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
        if !rest_trimmed.starts_with('"') {
            return Err(ParseError { line: lineno, msg: "array elements must be quoted strings" });
        }
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
}
