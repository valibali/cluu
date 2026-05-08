//! POSIX-style argument parser. Single-pass. Supports:
//! - clustered short flags: `-rfv`
//! - long options: `--all`, `--color=auto`
//! - `--` end-of-options marker
//! - required arg attachment: `-n5`, `-n 5`, `--lines=5`, `--lines 5`
//! - optional arg attachment: `-nVALUE`, `--name=VALUE`
//! - auto-generated `--help` / `--version` responses
//!
//! This module is `no_std`-compatible (uses `alloc::*` only). The `host-test`
//! feature enables the std test runner without touching this file.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Specifies whether an option takes a value, and how it's attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// Boolean flag; presence = true, absence = false.
    Flag,
    /// A value must follow: `-n 5`, `-n5`, `--lines 5`, `--lines=5`.
    RequiredArg,
    /// A value may be attached in-token (`-nVAL`, `--name=VAL`); otherwise
    /// the option is treated as a set flag with no value.
    OptionalArg,
}

/// A single option definition.
#[derive(Debug, Clone)]
pub struct Opt {
    /// Short character, e.g. `'n'`. `None` for long-only options.
    pub short: Option<char>,
    /// Long name without leading `--`, e.g. `"lines"`.
    pub long: &'static str,
    /// One-line description shown in `render_help`.
    pub help: &'static str,
    /// How the option interacts with a following value.
    pub kind: ArgKind,
}

/// Builder for a command's option specification.
#[derive(Debug, Default, Clone)]
pub struct Spec {
    /// Program name shown in usage line.
    pub program: &'static str,
    /// Version string returned for `--version`.
    pub version: &'static str,
    /// Usage synopsis (everything after `program` on the usage line).
    pub usage: &'static str,
    opts: Vec<Opt>,
}

impl Spec {
    /// Create an empty spec.
    pub fn new() -> Self {
        Spec {
            program: "",
            version: "0.1.0",
            usage: "",
            opts: Vec::new(),
        }
    }

    /// Set the program name.
    pub fn program(mut self, name: &'static str) -> Self {
        self.program = name;
        self
    }

    /// Set the version string (shown by `--version`).
    pub fn version(mut self, v: &'static str) -> Self {
        self.version = v;
        self
    }

    /// Set the usage synopsis shown after the program name.
    pub fn usage(mut self, u: &'static str) -> Self {
        self.usage = u;
        self
    }

    /// Add a boolean flag with both a short and long form.
    pub fn flag(mut self, short: char, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt {
            short: Some(short),
            long,
            help,
            kind: ArgKind::Flag,
        });
        self
    }

    /// Add a boolean flag with only a long form.
    pub fn long_flag(mut self, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt {
            short: None,
            long,
            help,
            kind: ArgKind::Flag,
        });
        self
    }

    /// Add an option that requires a value argument.
    pub fn required(mut self, short: char, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt {
            short: Some(short),
            long,
            help,
            kind: ArgKind::RequiredArg,
        });
        self
    }

    /// Add an option that may or may not have a value argument.
    pub fn optional(mut self, short: char, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt {
            short: Some(short),
            long,
            help,
            kind: ArgKind::OptionalArg,
        });
        self
    }
}

// ---------------------------------------------------------------------------
// Parse result
// ---------------------------------------------------------------------------

/// Parsed command-line arguments.
#[derive(Debug, Default)]
pub struct Parsed {
    /// Long-name → true for every flag that was set.
    flags: BTreeMap<String, bool>,
    /// Long-name → value for every RequiredArg/OptionalArg that carried a value.
    values: BTreeMap<String, String>,
    /// Non-option arguments (positional), in order.
    pub positional: Vec<String>,
}

impl Parsed {
    /// Returns `true` if the named flag was present.
    pub fn is_set(&self, long: &str) -> bool {
        *self.flags.get(long).unwrap_or(&false)
    }

    /// Returns the value for a `RequiredArg` or `OptionalArg` option, if any.
    pub fn value(&self, long: &str) -> Option<&str> {
        self.values.get(long).map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by [`parse`].
#[derive(Debug)]
pub enum CliError {
    /// An unrecognised `-x` or `--xyz` was encountered.
    UnknownOption(String),
    /// A `RequiredArg` option had no following value.
    MissingValue(String),
    /// `--help` was encountered; caller should print help and exit 0.
    HelpRequested,
    /// `--version` was encountered; caller should print version and exit 0.
    VersionRequested,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::UnknownOption(s) => write!(f, "unknown option: {}", s),
            CliError::MissingValue(s) => write!(f, "missing value for: {}", s),
            CliError::HelpRequested => write!(f, "(help)"),
            CliError::VersionRequested => write!(f, "(version)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse `argv` according to `spec`.
///
/// `argv[0]` is the program name and is always skipped.
///
/// # Return
/// - `Ok(Parsed)` on success.
/// - `Err(CliError::HelpRequested)` when `--help` is encountered.
/// - `Err(CliError::VersionRequested)` when `--version` is encountered.
/// - `Err(CliError::UnknownOption)` for an unrecognised flag.
/// - `Err(CliError::MissingValue)` when a required-value option has no value.
pub fn parse(spec: &Spec, argv: &[String]) -> Result<Parsed, CliError> {
    let mut out = Parsed::default();
    let mut i = 1usize; // skip argv[0] (program name)
    let mut after_dd = false; // encountered `--`

    while i < argv.len() {
        let a = &argv[i];

        // After `--` everything is positional, verbatim.
        if after_dd {
            out.positional.push(a.clone());
            i += 1;
            continue;
        }

        // End-of-options marker.
        if a == "--" {
            after_dd = true;
            i += 1;
            continue;
        }

        // Built-in responses — checked before looking at spec opts.
        if a == "--help" {
            return Err(CliError::HelpRequested);
        }
        if a == "--version" {
            return Err(CliError::VersionRequested);
        }

        // Long option: --name or --name=value
        if let Some(rest) = a.strip_prefix("--") {
            let (name, inline_value) = match rest.find('=') {
                Some(p) => (&rest[..p], Some(&rest[p + 1..])),
                None => (rest, None),
            };
            let opt = spec
                .opts
                .iter()
                .find(|o| o.long == name)
                .ok_or_else(|| CliError::UnknownOption(format!("--{}", name)))?;
            match opt.kind {
                ArgKind::Flag => {
                    out.flags.insert(opt.long.to_string(), true);
                }
                ArgKind::RequiredArg => {
                    let v = match inline_value {
                        Some(v) => v.to_string(),
                        None => {
                            i += 1;
                            argv.get(i).cloned().ok_or_else(|| {
                                CliError::MissingValue(format!("--{}", name))
                            })?
                        }
                    };
                    out.values.insert(opt.long.to_string(), v);
                }
                ArgKind::OptionalArg => {
                    if let Some(v) = inline_value {
                        out.values.insert(opt.long.to_string(), v.to_string());
                    } else {
                        out.flags.insert(opt.long.to_string(), true);
                    }
                }
            }
            i += 1;
            continue;
        }

        // Short option(s): -a  or  -rfv  or  -n5  or  -n 5
        if let Some(rest) = a.strip_prefix('-') {
            if rest.is_empty() {
                // Plain `-` is treated as a positional (stdin marker, GNU convention).
                out.positional.push(a.clone());
                i += 1;
                continue;
            }
            let mut chars = rest.chars().peekable();
            while let Some(c) = chars.next() {
                let opt = spec
                    .opts
                    .iter()
                    .find(|o| o.short == Some(c))
                    .ok_or_else(|| CliError::UnknownOption(format!("-{}", c)))?;
                match opt.kind {
                    ArgKind::Flag => {
                        out.flags.insert(opt.long.to_string(), true);
                        // Continue cluster loop (e.g. -rfv).
                    }
                    ArgKind::RequiredArg => {
                        // Remainder of cluster token is the value (-n5).
                        let attached: String = chars.collect();
                        let v = if !attached.is_empty() {
                            attached
                        } else {
                            // Value is the next token (-n 5).
                            i += 1;
                            argv.get(i).cloned().ok_or_else(|| {
                                CliError::MissingValue(format!("-{}", c))
                            })?
                        };
                        out.values.insert(opt.long.to_string(), v);
                        break; // consumed rest of cluster
                    }
                    ArgKind::OptionalArg => {
                        // Remainder of cluster token is the value, if present.
                        let attached: String = chars.collect();
                        if !attached.is_empty() {
                            out.values.insert(opt.long.to_string(), attached);
                        } else {
                            out.flags.insert(opt.long.to_string(), true);
                        }
                        break; // consumed rest of cluster
                    }
                }
            }
            i += 1;
            continue;
        }

        // Not a flag — positional argument.
        out.positional.push(a.clone());
        i += 1;
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Help renderer
// ---------------------------------------------------------------------------

/// Produce a formatted usage + options block for display on stdout.
///
/// Output format:
/// ```text
/// Usage: PROGRAM USAGE
///
/// Options:
///   -X, --long  help text
///       --long  help text (long-only)
/// ```
pub fn render_help(spec: &Spec) -> String {
    let mut s = String::new();
    s.push_str("Usage: ");
    s.push_str(spec.program);
    if !spec.usage.is_empty() {
        s.push(' ');
        s.push_str(spec.usage);
    }
    s.push_str("\n\nOptions:\n");
    // Built-in options come first.
    s.push_str("      --help     display this help and exit\n");
    s.push_str("      --version  output version information and exit\n");
    for o in &spec.opts {
        s.push_str("  ");
        if let Some(c) = o.short {
            s.push('-');
            s.push(c);
            s.push_str(", ");
        } else {
            s.push_str("    ");
        }
        s.push_str("--");
        s.push_str(o.long);
        // Pad to column ~20 for alignment (best-effort).
        let used = 2 + if o.short.is_some() { 4 } else { 4 } + 2 + o.long.len();
        let pad = if used < 20 { 20 - used } else { 2 };
        for _ in 0..pad {
            s.push(' ');
        }
        s.push_str(o.help);
        s.push('\n');
    }
    s
}
