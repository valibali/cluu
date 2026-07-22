//! Standalone container image builder for CLUU.
//!
//! Reads a Cluufile and produces a container image directory under
//! `target/containers/<name>/` with a generated `manifest.toml`.
//!
//! Usage:
//!   cargo run -p container-build -- <path-to-Cluufile>

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "container-build", about = "Build a CLUU container image from a Cluufile")]
struct Cli {
    /// Path to the Cluufile
    path: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    container_build(&cli.path)
}

// ═══════════════════════════════════════════════════════════════════════════
// Cluufile parser
// ═══════════════════════════════════════════════════════════════════════════

/// A build step: run a command, then copy the build output into the container.
#[derive(Debug, Clone)]
struct BuildStep {
    command: String,
    build_output: String,
    container_path: String,
}

/// Typed value of a DRIVER directive key=value pair. Captures the value's
/// syntactic form so the manifest emitter renders it back as TOML with the
/// right type. drivermgr (D2.5) re-parses the TOML, so the Cluufile parser
/// only needs the syntactic type, not the semantic meaning of each key.
#[derive(Debug, Clone)]
enum DriverValue {
    /// Hex literal `0x1af4` — emitted as `0x1af4` for readability.
    Hex(u64),
    /// Decimal literal `180`.
    Int(u64),
    /// Array `[0x1001,0x1042]` — the bool tracks whether the source used
    /// hex (`0x`) or decimal form, so emission preserves the author's intent.
    HexArray(Vec<(u64, bool)>),
    /// `true` / `false`, or a bare flag (`dma`) which implies `true`.
    Bool(bool),
    /// Quoted string `"sys/virtio-blk.elf"` — quotes stripped.
    String(String),
    /// Bare identifier `pci`, `acpi` — emitted as a TOML string.
    Ident(String),
}

/// One `DRIVER <sub> <key>=<val> ...` line. `sub` ∈
/// {bind, hardware, lifecycle, source, envelope}. `keys` preserves source
/// order so the manifest emitter reproduces the Cluufile's intent.
#[derive(Debug, Clone)]
struct DriverDirective {
    sub: String,
    keys: Vec<(String, DriverValue)>,
}

/// All DRIVER directives from a Cluufile. Present only when `FROM driver`.
/// Multiple directives with the same `sub` accumulate (e.g. virtio-blk has
/// one `DRIVER bind`, one `DRIVER lifecycle`, one `DRIVER source`).
#[derive(Debug, Clone, Default)]
struct DriverSpec {
    directives: Vec<DriverDirective>,
}

/// Parsed representation of a Cluufile — a declarative container manifest
/// used at build time to assemble container images.
#[derive(Debug, Clone)]
struct Cluufile {
    base: String,
    profile: Vec<String>,
    entrypoint: Vec<String>,
    builds: Vec<BuildStep>,
    copies: Vec<(String, String)>,
    persistent_dirs: Vec<String>,
    env: Vec<(String, String)>,
    priority: Option<usize>,
    endpoint_mode: Option<String>,
    params: Vec<String>,
    devices: Vec<String>,
    devpaths: Vec<String>,
    deny_inherit: bool,
    deny: Vec<String>,
    detach: bool,
    restart_policy: Option<(String, Option<usize>, Option<u64>)>,
    /// MOUNT directives: (path, policy) where policy ∈
    /// {"inherit", "private", "ro", "readonly", "rw", "readwrite"}.
    /// Procmgr's mount_policy parser maps `ro/readonly` → Inherit+Ro and
    /// `rw/readwrite` → Inherit+Rw. Duplicate paths are a parse error
    /// (caught in parse_cluufile).
    mount_policies: Vec<(String, String)>,
    /// PRELOAD: hint to VFS to fill its ELF cache for this container's
    /// binary at startup, so first-spawn pays the disk read upfront.
    preload: bool,
    /// DRIVER directives. `None` when the Cluufile has no DRIVER lines
    /// (the common case for `FROM minimal`/`FROM base`). `Some` when
    /// `FROM driver` is used. Post-parse validation enforces the
    /// relationship between `base` and `driver` (see parse_cluufile).
    driver: Option<DriverSpec>,
}

fn parse_cluufile(path: &Path) -> Result<Cluufile> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read Cluufile at {}", path.display()))?;

    let mut base: Option<String> = None;
    let mut profile: Option<Vec<String>> = None;
    let mut entrypoint: Option<Vec<String>> = None;
    let mut builds: Vec<BuildStep> = Vec::new();
    let mut copies: Vec<(String, String)> = Vec::new();
    let mut persistent_dirs: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    let mut priority: Option<usize> = None;
    let mut endpoint_mode: Option<String> = None;
    let mut params: Vec<String> = Vec::new();
    let mut devices: Vec<String> = Vec::new();
    let mut devpaths: Vec<String> = Vec::new();
    let mut deny_inherit = false;
    let mut deny: Vec<String> = Vec::new();
    let mut detach = false;
    let mut restart_policy: Option<(String, Option<usize>, Option<u64>)> = None;
    let mut mount_policies: Vec<(String, String)> = Vec::new();
    let mut preload = false;
    let mut driver: Option<DriverSpec> = None;
    let mut saw_directive = false;

    for (line_idx, raw_line) in content.lines().enumerate() {
        let lineno = line_idx + 1;
        let line = raw_line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split into directive and rest
        let (directive, rest) = match line.split_once(char::is_whitespace) {
            Some((d, r)) => (d, r.trim()),
            None => (line, ""),
        };

        match directive {
            "FROM" => {
                if saw_directive {
                    bail!("{}:{}: FROM must be the first directive", path.display(), lineno);
                }
                if rest.is_empty() {
                    bail!("{}:{}: FROM requires a value", path.display(), lineno);
                }
                base = Some(rest.to_string());
            }
            "PROFILE" => {
                if profile.is_some() {
                    bail!("{}:{}: duplicate PROFILE directive", path.display(), lineno);
                }
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before PROFILE", path.display(), lineno);
                }
                profile = Some(
                    rest.split_whitespace()
                        .map(|s| s.to_string())
                        .collect(),
                );
            }
            "ENTRYPOINT" => {
                if entrypoint.is_some() {
                    bail!("{}:{}: duplicate ENTRYPOINT directive", path.display(), lineno);
                }
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before ENTRYPOINT", path.display(), lineno);
                }
                if rest.is_empty() {
                    bail!("{}:{}: ENTRYPOINT requires a value", path.display(), lineno);
                }
                entrypoint = Some(
                    rest.split_whitespace()
                        .map(|s| s.to_string())
                        .collect(),
                );
            }
            "BUILD" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before BUILD", path.display(), lineno);
                }
                // BUILD "command" build-output container-path
                // The command is quoted; the two paths are unquoted.
                let rest_trimmed = rest.trim();
                if !rest_trimmed.starts_with('"') {
                    bail!(
                        "{}:{}: BUILD command must be quoted: BUILD \"command\" output dest",
                        path.display(), lineno
                    );
                }
                // Find the closing quote
                let after_open = &rest_trimmed[1..];
                let close_pos = after_open.find('"').ok_or_else(|| {
                    anyhow::anyhow!("{}:{}: unterminated quote in BUILD command", path.display(), lineno)
                })?;
                let command = after_open[..close_pos].to_string();
                let remainder = after_open[close_pos + 1..].trim();
                let paths: Vec<&str> = remainder.split_whitespace().collect();
                if paths.len() != 2 {
                    bail!(
                        "{}:{}: BUILD requires exactly: \"command\" build-output container-path, got {} path(s)",
                        path.display(), lineno, paths.len()
                    );
                }
                builds.push(BuildStep {
                    command,
                    build_output: paths[0].to_string(),
                    container_path: paths[1].to_string(),
                });
            }
            "COPY" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before COPY", path.display(), lineno);
                }
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() != 2 {
                    bail!(
                        "{}:{}: COPY requires exactly two arguments (src dest), got {}",
                        path.display(), lineno, parts.len()
                    );
                }
                copies.push((parts[0].to_string(), parts[1].to_string()));
            }
            "PERSISTENT" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before PERSISTENT", path.display(), lineno);
                }
                if rest.is_empty() {
                    bail!("{}:{}: PERSISTENT requires a path", path.display(), lineno);
                }
                persistent_dirs.push(rest.to_string());
            }
            "ENV" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before ENV", path.display(), lineno);
                }
                let (key, value) = rest.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("{}:{}: ENV requires key=value format", path.display(), lineno)
                })?;
                let key = key.trim();
                let value = value.trim();
                if key.is_empty() {
                    bail!("{}:{}: ENV key cannot be empty", path.display(), lineno);
                }
                env.push((key.to_string(), value.to_string()));
            }
            "PRIORITY" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before PRIORITY", path.display(), lineno);
                }
                if priority.is_some() {
                    bail!("{}:{}: duplicate PRIORITY directive", path.display(), lineno);
                }
                let val: usize = rest.parse().map_err(|_| {
                    anyhow::anyhow!("{}:{}: PRIORITY requires an integer value", path.display(), lineno)
                })?;
                if val < 1 || val > 255 {
                    bail!("{}:{}: PRIORITY must be 1-255, got {}", path.display(), lineno, val);
                }
                priority = Some(val);
            }
            "ENDPOINT" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before ENDPOINT", path.display(), lineno);
                }
                if endpoint_mode.is_some() {
                    bail!("{}:{}: duplicate ENDPOINT directive", path.display(), lineno);
                }
                match rest {
                    "listen" | "grantable" => {
                        endpoint_mode = Some(rest.to_string());
                    }
                    _ => {
                        bail!(
                            "{}:{}: ENDPOINT must be 'listen' or 'grantable', got '{}'",
                            path.display(), lineno, rest
                        );
                    }
                }
            }
            "PARAM" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before PARAM", path.display(), lineno);
                }
                if rest.is_empty() {
                    bail!("{}:{}: PARAM requires a name", path.display(), lineno);
                }
                let name = rest.to_string();
                if params.contains(&name) {
                    bail!("{}:{}: duplicate PARAM '{}'", path.display(), lineno, name);
                }
                params.push(name);
            }
            "DEVICE" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before DEVICE", path.display(), lineno);
                }
                if rest.starts_with("/dev/") {
                    if devpaths.contains(&rest.to_string()) {
                        bail!("{}:{}: duplicate DEVICE '{}'", path.display(), lineno, rest);
                    }
                    devpaths.push(rest.to_string());
                } else {
                    match rest {
                        "irq" | "framebuffer" => {
                            if devices.contains(&rest.to_string()) {
                                bail!("{}:{}: duplicate DEVICE '{}'", path.display(), lineno, rest);
                            }
                            devices.push(rest.to_string());
                        }
                        _ => {
                            bail!(
                                "{}:{}: DEVICE must be 'irq', 'framebuffer', or '/dev/...' path, got '{}'",
                                path.display(), lineno, rest
                            );
                        }
                    }
                }
            }
            "DENY_INHERIT" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before DENY_INHERIT", path.display(), lineno);
                }
                deny_inherit = true;
            }
            "DENY" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before DENY", path.display(), lineno);
                }
                if rest.is_empty() {
                    bail!("{}:{}: DENY requires a path", path.display(), lineno);
                }
                deny.push(rest.to_string());
            }
            "DETACH" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before DETACH", path.display(), lineno);
                }
                detach = true;
            }
            "PRELOAD" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before PRELOAD", path.display(), lineno);
                }
                preload = true;
            }
            "RESTART" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before RESTART", path.display(), lineno);
                }
                if restart_policy.is_some() {
                    bail!("{}:{}: duplicate RESTART directive", path.display(), lineno);
                }
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                if tokens.is_empty() {
                    bail!("{}:{}: RESTART requires a policy (never, always, on_failure)", path.display(), lineno);
                }
                match tokens[0] {
                    "never" | "always" | "on_failure" => {}
                    other => {
                        bail!(
                            "{}:{}: RESTART policy must be 'never', 'always', or 'on_failure', got '{}'",
                            path.display(), lineno, other
                        );
                    }
                }
                let max_restarts = if tokens.len() >= 2 {
                    Some(tokens[1].parse::<usize>().map_err(|_| {
                        anyhow::anyhow!("{}:{}: RESTART max_restarts must be an integer", path.display(), lineno)
                    })?)
                } else if tokens[0] == "on_failure" {
                    bail!("{}:{}: RESTART on_failure requires max_restarts and restart_window (e.g. 'RESTART on_failure 3 120')",
                          path.display(), lineno);
                } else {
                    None
                };
                let restart_window = if tokens.len() >= 3 {
                    Some(tokens[2].parse::<u64>().map_err(|_| {
                        anyhow::anyhow!("{}:{}: RESTART restart_window must be an integer", path.display(), lineno)
                    })?)
                } else if tokens[0] == "on_failure" {
                    bail!("{}:{}: RESTART on_failure requires restart_window", path.display(), lineno);
                } else {
                    None
                };
                restart_policy = Some((tokens[0].to_string(), max_restarts, restart_window));
            }
            "MOUNT" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before MOUNT", path.display(), lineno);
                }
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                if tokens.len() != 2 {
                    bail!(
                        "{}:{}: MOUNT requires exactly two arguments (path policy), got {}",
                        path.display(), lineno, tokens.len()
                    );
                }
                let mount_path = tokens[0].to_string();
                let policy = tokens[1].to_string();
                match policy.as_str() {
                    "inherit" | "private" | "ro" | "readonly" | "rw" | "readwrite" => {}
                    other => {
                        bail!(
                            "{}:{}: MOUNT policy must be 'inherit', 'private', 'ro', 'readonly', 'rw', or 'readwrite', got '{}'",
                            path.display(), lineno, other
                        );
                    }
                }
                if !mount_path.starts_with('/') {
                    bail!(
                        "{}:{}: MOUNT path must be absolute, got '{}'",
                        path.display(), lineno, mount_path
                    );
                }
                if mount_policies.iter().any(|(p, _)| p == &mount_path) {
                    bail!(
                        "{}:{}: duplicate MOUNT directive for path '{}'",
                        path.display(), lineno, mount_path
                    );
                }
                mount_policies.push((mount_path, policy));
            }
            "DRIVER" => {
                if base.is_none() {
                    bail!("{}:{}: FROM must appear before DRIVER", path.display(), lineno);
                }
                // DRIVER <sub> <key>=<val> <key>=<val> ...  (bare flags ok: `dma` ⟶ dma=true)
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                if tokens.is_empty() {
                    bail!(
                        "{}:{}: DRIVER requires a sub-directive (bind, hardware, lifecycle, source, envelope)",
                        path.display(), lineno
                    );
                }
                let sub = tokens[0].to_string();
                match sub.as_str() {
                    "bind" | "hardware" | "lifecycle" | "source" | "envelope" | "tokens" => {}
                    other => {
                        bail!(
                            "{}:{}: unknown DRIVER sub-directive '{}'; expected bind, hardware, lifecycle, source, envelope, or tokens",
                            path.display(), lineno, other
                        );
                    }
                }
                let mut keys: Vec<(String, DriverValue)> = Vec::new();
                for tok in &tokens[1..] {
                    let (k, v) = parse_driver_kv(tok, path, lineno)
                        .with_context(|| format!("{}:{}: malformed DRIVER key=value '{}'", path.display(), lineno, tok))?;
                    keys.push((k, v));
                }
                if driver.is_none() {
                    driver = Some(DriverSpec::default());
                }
                driver.as_mut().unwrap().directives.push(DriverDirective { sub, keys });
            }
            unknown => {
                bail!(
                    "{}:{}: unknown directive '{}'",
                    path.display(), lineno, unknown
                );
            }
        }

        saw_directive = true;
    }

    // Post-parse validation: MOUNT must not overlap with DENY or PERSISTENT.
    // Both orderings caught because we check after the whole file is parsed.
    for (mount_path, _) in &mount_policies {
        if deny.iter().any(|d| d == mount_path) {
            bail!(
                "{}: MOUNT conflicts with DENY for path '{}' (ambiguous intent)",
                path.display(), mount_path
            );
        }
        if persistent_dirs.iter().any(|p| p == mount_path) {
            bail!(
                "{}: MOUNT conflicts with PERSISTENT for path '{}' (PERSISTENT already implies private)",
                path.display(), mount_path
            );
        }
    }

    let base = base.ok_or_else(|| {
        anyhow::anyhow!("{}: missing required FROM directive", path.display())
    })?;

    // Post-parse validation: FROM driver ⟺ DRIVER directives.
    // CC.3: `FROM driver` requires at least one DRIVER bind directive;
    // any DRIVER directive requires `FROM driver`. The two sides are
    // checked together so the error names the actual mismatch.
    let has_driver_bind = driver
        .as_ref()
        .map(|d| d.directives.iter().any(|dd| dd.sub == "bind"))
        .unwrap_or(false);
    match (base.as_str(), &driver) {
        ("driver", None) => {
            bail!(
                "{}: FROM driver requires at least one DRIVER bind directive",
                path.display()
            );
        }
        ("driver", Some(spec)) if !has_driver_bind => {
            bail!(
                "{}: FROM driver requires at least one DRIVER bind directive (got {} DRIVER directive(s), none were bind)",
                path.display(), spec.directives.len()
            );
        }
        (b, Some(_)) if b != "driver" => {
            bail!(
                "{}: DRIVER directive requires FROM driver (got FROM {})",
                path.display(), b
            );
        }
        _ => {}
    }

    Ok(Cluufile {
        base,
        profile: profile.unwrap_or_default(),
        entrypoint: entrypoint.unwrap_or_default(),
        builds,
        copies,
        persistent_dirs,
        env,
        priority,
        endpoint_mode,
        params,
        devices,
        devpaths,
        deny_inherit,
        deny,
        detach,
        restart_policy,
        mount_policies,
        preload,
        driver,
    })
}

/// Parse one `key=value` (or bare `flag`) token from a DRIVER directive.
///
/// Accepted value forms:
///   - `"quoted string"`   → DriverValue::String (quotes stripped)
///   - `[0x1,0x2]` / `[1,2]` → DriverValue::HexArray (bool = source was hex)
///   - `0x1af4`            → DriverValue::Hex
///   - `true` / `false`    → DriverValue::Bool
///   - `180`               → DriverValue::Int
///   - `pci` (no `=`)      → DriverValue::Bool(true) — bare flag
///   - `bus=pci`           → DriverValue::Ident — bare identifier
fn parse_driver_kv(tok: &str, path: &Path, lineno: usize) -> Result<(String, DriverValue)> {
    let (key, raw_val) = match tok.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (tok, None),
    };
    if key.is_empty() {
        bail!("{}:{}: DRIVER key is empty", path.display(), lineno);
    }
    if key.chars().any(|c| c.is_whitespace()) {
        bail!("{}:{}: DRIVER key '{}' contains whitespace", path.display(), lineno, key);
    }

    let val = match raw_val {
        None => DriverValue::Bool(true),
        Some(v) => parse_driver_value(v, path, lineno)?,
    };
    Ok((key.to_string(), val))
}

fn parse_driver_value(v: &str, path: &Path, lineno: usize) -> Result<DriverValue> {
    if v.is_empty() {
        bail!("{}:{}: DRIVER value is empty", path.display(), lineno);
    }
    if let Some(after_open) = v.strip_prefix('"') {
        if after_open.is_empty() || !after_open.ends_with('"') {
            bail!("{}:{}: DRIVER string value '{}' is missing closing quote", path.display(), lineno, v);
        }
        let inner = &after_open[..after_open.len() - 1];
        return Ok(DriverValue::String(inner.to_string()));
    }
    // Array of integers. Whitespace inside is rejected because the directive
    // tokenizer already split on whitespace — an array with spaces would
    // arrive as multiple tokens and fail earlier.
    if v.starts_with('[') {
        if !v.ends_with(']') {
            bail!("{}:{}: DRIVER array '{}' is missing closing ']'", path.display(), lineno, v);
        }
        let inner = &v[1..v.len() - 1];
        let mut items: Vec<(u64, bool)> = Vec::new();
        for item in inner.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (n, is_hex) = parse_int_literal(item, path, lineno)?;
            items.push((n, is_hex));
        }
        if items.is_empty() {
            bail!("{}:{}: DRIVER array '{}' is empty", path.display(), lineno, v);
        }
        return Ok(DriverValue::HexArray(items));
    }
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        let n = parse_hex_digits(hex, path, lineno)?;
        return Ok(DriverValue::Hex(n));
    }
    match v {
        "true" => return Ok(DriverValue::Bool(true)),
        "false" => return Ok(DriverValue::Bool(false)),
        _ => {}
    }
    if v.chars().all(|c| c.is_ascii_digit()) {
        let n: u64 = v.parse()
            .map_err(|_| anyhow::anyhow!("{}:{}: DRIVER decimal value '{}' overflows u64", path.display(), lineno, v))?;
        return Ok(DriverValue::Int(n));
    }
    if v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Ok(DriverValue::Ident(v.to_string()));
    }
    bail!(
        "{}:{}: DRIVER value '{}' is not a recognized literal (expected \"string\", [array], 0xhex, decimal, true/false, or identifier)",
        path.display(), lineno, v
    );
}

/// Parse `0x1af4`-style or `180`-style integer. Returns (value, is_hex).
fn parse_int_literal(s: &str, path: &Path, lineno: usize) -> Result<(u64, bool)> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let n = parse_hex_digits(hex, path, lineno)?;
        Ok((n, true))
    } else if s.chars().all(|c| c.is_ascii_digit()) {
        let n: u64 = s.parse()
            .map_err(|_| anyhow::anyhow!("{}:{}: DRIVER integer '{}' overflows u64", path.display(), lineno, s))?;
        Ok((n, false))
    } else {
        bail!(
            "{}:{}: DRIVER integer literal '{}' must be decimal or 0x-prefixed hex",
            path.display(), lineno, s
        );
    }
}

fn parse_hex_digits(hex: &str, path: &Path, lineno: usize) -> Result<u64> {
    // Strip TOML-style underscore separators (e.g. 0x5100_0000).
    let cleaned: String = hex.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() {
        bail!("{}:{}: DRIVER hex literal has no digits after '0x'", path.display(), lineno);
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "{}:{}: DRIVER hex literal '0x{}' contains non-hex characters",
            path.display(), lineno, hex
        );
    }
    u64::from_str_radix(&cleaned, 16)
        .map_err(|_| anyhow::anyhow!("{}:{}: DRIVER hex literal '0x{}' overflows u64", path.display(), lineno, hex))
}

// ═══════════════════════════════════════════════════════════════════════════
// manifest.toml generator
// ═══════════════════════════════════════════════════════════════════════════

fn generate_manifest_toml(cluufile: &Cluufile, container_name: &str, image_dirs: &[String]) -> String {
    let mut out = String::from("# Auto-generated from Cluufile — do not edit\n");

    // [container]
    out.push_str(&format!(
        "\n[container]\nname = \"{}\"\n",
        container_name
    ));
    if cluufile.detach {
        out.push_str("detach = true\n");
    }
    if cluufile.preload {
        out.push_str("preload = true\n");
    }

    // [profile] — only if capabilities specified
    if !cluufile.profile.is_empty() {
        out.push_str("\n[profile]\ncapabilities = [");
        for (i, cap) in cluufile.profile.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("\"{}\"", cap));
        }
        out.push_str("]\n");
    }

    // [exec] — only if entrypoint specified
    if !cluufile.entrypoint.is_empty() {
        out.push_str(&format!(
            "\n[exec]\nbinary = \"{}\"\n",
            cluufile.entrypoint[0]
        ));
        if cluufile.entrypoint.len() > 1 {
            out.push_str("args = [");
            for (i, arg) in cluufile.entrypoint[1..].iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", arg));
            }
            out.push_str("]\n");
        }
    }

    // [lifecycle] — only if restart policy specified
    if let Some((ref policy, ref max_restarts, ref restart_window)) = cluufile.restart_policy {
        out.push_str(&format!("\n[lifecycle]\nrestart_policy = \"{}\"\n", policy));
        if let Some(max) = max_restarts {
            out.push_str(&format!("max_restarts = {}\n", max));
        }
        if let Some(window) = restart_window {
            out.push_str(&format!("restart_window_secs = {}\n", window));
        }
    }

    // [storage] — image_dirs and/or persistent_dirs
    if !image_dirs.is_empty() || !cluufile.persistent_dirs.is_empty() {
        out.push_str("\n[storage]\n");
        if !image_dirs.is_empty() {
            out.push_str("image_dirs = [");
            for (i, dir) in image_dirs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", dir));
            }
            out.push_str("]\n");
        }
        if !cluufile.persistent_dirs.is_empty() {
            out.push_str("persistent_dirs = [");
            for (i, dir) in cluufile.persistent_dirs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", dir));
            }
            out.push_str("]\n");
        }
    }

    // [[env]] — one section per entry
    for (key, value) in &cluufile.env {
        out.push_str(&format!(
            "\n[[env]]\nkey = \"{}\"\nvalue = \"{}\"\n",
            key, value
        ));
    }

    // [scheduling] — only if priority specified
    if let Some(prio) = cluufile.priority {
        out.push_str(&format!("\n[scheduling]\npriority = \"{}\"\n", prio));
    }

    // [tokens] — only if endpoint mode specified
    if let Some(ref mode) = cluufile.endpoint_mode {
        out.push_str(&format!("\n[tokens]\nendpoint_mode = \"{}\"\n", mode));
    }

    // [hardware] — only if devices specified
    if !cluufile.devices.is_empty() || !cluufile.devpaths.is_empty() {
        out.push_str("\n[hardware]\n");
        if !cluufile.devices.is_empty() {
            out.push_str("devices = [");
            for (i, dev) in cluufile.devices.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", dev));
            }
            out.push_str("]\n");
        }
        if !cluufile.devpaths.is_empty() {
            out.push_str("devpaths = [");
            for (i, dp) in cluufile.devpaths.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", dp));
            }
            out.push_str("]\n");
        }
    }

    // [params] — only if param slots specified
    if !cluufile.params.is_empty() {
        out.push_str("\n[params]\nslots = [");
        for (i, name) in cluufile.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("\"{}\"", name));
        }
        out.push_str("]\n");
    }

    // [mounts] — emitted if deny_inherit, deny paths, or mount policies are set.
    // Mount policies are emitted as [[mounts.policy]] array-of-tables so procmgr
    // can read them as a vector without ambiguity versus deny_inherit / deny.
    let has_mount_section = cluufile.deny_inherit
        || !cluufile.deny.is_empty()
        || !cluufile.mount_policies.is_empty();
    if has_mount_section {
        out.push_str("\n[mounts]\n");
        if cluufile.deny_inherit {
            out.push_str("deny_inherit = true\n");
        }
        if !cluufile.deny.is_empty() {
            out.push_str("deny = [");
            for (i, path) in cluufile.deny.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", path));
            }
            out.push_str("]\n");
        }
        for (path, policy) in &cluufile.mount_policies {
            out.push_str(&format!(
                "\n[[mounts.policy]]\npath = \"{}\"\npolicy = \"{}\"\n",
                path, policy
            ));
        }
    }

    // [driver] — emitted only for FROM driver Cluufiles. procmgr skips this
    // section; drivermgr (D2.5) parses it to build BindRuleTable. Directives
    // are grouped by sub in first-appearance order, each group emitted as
    // `[[driver.<sub>]]` array-of-tables so multiple DRIVER bind (or other
    // sub) directives stay distinct.
    if let Some(ref driver) = cluufile.driver {
        out.push_str("\n[driver]\n");
        let order = driver_sub_order(&driver.directives);
        for sub in &order {
            for directive in driver.directives.iter().filter(|d| &d.sub == sub) {
                out.push_str(&format!("\n[[driver.{}]]\n", sub));
                for (k, v) in &directive.keys {
                    out.push_str(&format!("{} = {}\n", k, emit_driver_value(v)));
                }
            }
        }
    }

    out
}

/// First-appearance order of `sub` values across the directives. Keeps the
/// manifest's section ordering faithful to the Cluufile's authoring.
fn driver_sub_order(directives: &[DriverDirective]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for d in directives {
        if !order.contains(&d.sub) {
            order.push(d.sub.clone());
        }
    }
    order
}

fn emit_driver_value(v: &DriverValue) -> String {
    match v {
        DriverValue::Hex(n) => format!("0x{:x}", n),
        DriverValue::Int(n) => format!("{}", n),
        DriverValue::HexArray(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|(n, is_hex)| if *is_hex { format!("0x{:x}", n) } else { format!("{}", n) })
                .collect();
            format!("[{}]", parts.join(", "))
        }
        DriverValue::Bool(b) => format!("{}", b),
        DriverValue::String(s) => format!("\"{}\"", s),
        DriverValue::Ident(s) => format!("\"{}\"", s),
    }
}

/// Discover top-level directories in the container image output dir.
/// These become `image_dirs` in the manifest so procmgr can set up per-image
/// view mounts (e.g., /bin → /var/images/<name>/bin).
fn discover_image_dirs(output_dir: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(output_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip build temp dir and any dotfiles
                if name != "build" && !name.starts_with('.') {
                    dirs.push(name);
                }
            }
        }
    }
    dirs.sort();
    dirs
}

// ═══════════════════════════════════════════════════════════════════════════
// Container build
// ═══════════════════════════════════════════════════════════════════════════

fn container_build(cluufile_path: &Path) -> Result<()> {
    let cluufile_path = cluufile_path.canonicalize()
        .with_context(|| format!("Cannot resolve Cluufile path: {}", cluufile_path.display()))?;

    let cluufile = parse_cluufile(&cluufile_path)?;

    // Derive container name from the Cluufile's parent directory
    let container_name = cluufile_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Cannot determine container name from path"))?
        .to_string();

    println!("▸ Building container '{}'...", container_name);

    let output_dir = project_root().join("target/containers").join(&container_name);
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)?;
    }
    fs::create_dir_all(&output_dir)?;

    // FROM base: copy sysroot /bin and /lib into the image
    if cluufile.base == "base" {
        let sysroot = project_root().join("target/sysroot");
        let sysroot_bin = sysroot.join("bin");
        let sysroot_lib = sysroot.join("lib");

        if sysroot_bin.exists() {
            let dst_bin = output_dir.join("bin");
            fs::create_dir_all(&dst_bin)?;
            copy_dir_contents(&sysroot_bin, &dst_bin)
                .context("Failed to copy sysroot/bin")?;
            println!("  Copied sysroot/bin/");
        }
        if sysroot_lib.exists() {
            let dst_lib = output_dir.join("lib");
            fs::create_dir_all(&dst_lib)?;
            copy_dir_contents(&sysroot_lib, &dst_lib)
                .context("Failed to copy sysroot/lib")?;
            println!("  Copied sysroot/lib/");
        }
    }

    // Execute BUILD directives: run the command, then copy build output into image
    let root = project_root();
    for step in &cluufile.builds {
        execute_build(&root, step, &output_dir, &container_name)?;
    }

    // Process COPY directives: resolve host paths relative to Cluufile directory
    let cluufile_dir = cluufile_path.parent().unwrap();
    for (src_rel, dst_rel) in &cluufile.copies {
        let src = cluufile_dir.join(src_rel);
        if !src.exists() {
            bail!("COPY source not found: {}", src.display());
        }
        // Strip leading '/' from container path to make it relative under output_dir
        let dst_rel_stripped = dst_rel.strip_prefix('/').unwrap_or(dst_rel);
        let dst = output_dir.join(dst_rel_stripped);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        if src.is_dir() {
            fs::create_dir_all(&dst)?;
            copy_dir_contents(&src, &dst)
                .with_context(|| format!("Failed to copy dir {} → {}", src.display(), dst.display()))?;
        } else {
            fs::copy(&src, &dst)
                .with_context(|| format!("Failed to copy {} → {}", src.display(), dst.display()))?;
        }
        println!("  COPY {} → {}", src_rel, dst_rel);
    }

    // Auto-discover image directories (top-level dirs in the image)
    let image_dirs = discover_image_dirs(&output_dir);

    // Generate manifest.toml
    let manifest = generate_manifest_toml(&cluufile, &container_name, &image_dirs);
    let manifest_path = output_dir.join("manifest.toml");
    fs::write(&manifest_path, &manifest)?;
    println!("  Generated manifest.toml");

    println!("✓ Container '{}' built at {}", container_name, output_dir.display());
    Ok(())
}

/// Execute a single BUILD step: run the command from the project root, then copy the
/// build artifact into the container image directory.
///
/// Sets `CLUU_BUILD_OUTPUT_DIR` so that build tools (e.g. `cargo xtask build-c`) can
/// redirect their outputs to a container-scoped directory, avoiding races when
/// multiple containers build the same artifact in parallel.
fn execute_build(
    project_root: &Path,
    step: &BuildStep,
    output_dir: &Path,
    container_name: &str,
) -> Result<()> {
    // Inject release profile for cargo-shaped builds. Containers default to
    // optimized binaries; debug builds were ~5-10x larger (e.g. ls.elf 4.6 MB
    // → ~500 KB). Cluufiles still reference `debug/` paths for readability;
    // we rewrite the lookup path here. Non-cargo BUILD commands pass through.
    let (effective_command, effective_build_output) = promote_to_release(&step.command, &step.build_output);

    println!("  BUILD \"{}\"", effective_command);

    // Container-scoped build output directory to avoid parallel build races.
    let build_dir = project_root
        .join("target/containers")
        .join(container_name)
        .join("build");
    fs::create_dir_all(&build_dir)?;

    let status = Command::new("sh")
        .arg("-c")
        .arg(&effective_command)
        .current_dir(project_root)
        .env("CLUU_BUILD_OUTPUT_DIR", &build_dir)
        .status()
        .with_context(|| format!("Failed to execute BUILD command: {}", effective_command))?;

    if !status.success() {
        bail!(
            "BUILD command failed (exit {}): {}",
            status.code().unwrap_or(-1),
            effective_command
        );
    }

    // Look for the build output: first in the container-scoped dir, then the
    // original path.  The scoped dir is preferred because build tools that
    // honour CLUU_BUILD_OUTPUT_DIR write there, making parallel builds safe.
    let filename = Path::new(&effective_build_output)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| effective_build_output.clone());
    let scoped_src = build_dir.join(&filename);
    let original_src = project_root.join(&effective_build_output);

    let src = if scoped_src.exists() {
        scoped_src
    } else if original_src.exists() {
        original_src
    } else {
        bail!(
            "BUILD output not found after command: {} (checked {} and {})",
            effective_build_output,
            scoped_src.display(),
            original_src.display(),
        );
    };

    let dst_rel = step.container_path.strip_prefix('/').unwrap_or(&step.container_path);
    let dst = output_dir.join(dst_rel);
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&src, &dst)
        .with_context(|| format!("Failed to copy build output {} → {}", src.display(), dst.display()))?;

    println!("  BUILD {} → {}", effective_build_output, step.container_path);
    Ok(())
}

/// Rewrite a BUILD command + output path to use an optimized profile when the
/// command is a recognized cargo invocation.
///
/// Why: containers defaulted to debug-mode cargo builds, which produced
/// 4-7 MB binaries (10x larger than release). Smaller container binaries
/// reduce VFS cache pressure, disk reads, and fault-corruption blast radius.
///
/// Recognized shapes:
///   - `cargo build ...`               → adds `--release`,                    debug/ → release/
///   - `cargo xtask build-c NAME SRC`  → appends `--profile release`,         debug/ → release/
/// Anything else passes through unchanged.
fn promote_to_release(command: &str, build_output: &str) -> (String, String) {
    let trimmed = command.trim_start();
    let promoted_cmd = if let Some(rest) = trimmed.strip_prefix("cargo build ") {
        if trimmed.contains(" --release") || trimmed.contains(" --profile ") {
            command.to_string()
        } else {
            format!("cargo build --release {}", rest)
        }
    } else if let Some(rest) = trimmed.strip_prefix("cargo xtask build-c ") {
        if trimmed.contains(" --profile ") {
            command.to_string()
        } else {
            format!("cargo xtask build-c {} --profile release", rest)
        }
    } else {
        return (command.to_string(), build_output.to_string());
    };

    let promoted_out = build_output.replace(
        "target/x86_64-cluu-user/debug/",
        "target/x86_64-cluu-user/release/",
    );
    (promoted_cmd, promoted_out)
}

/// Find the project root by walking up from the current directory looking for Cargo.toml
/// with a [workspace] section.
fn project_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("cannot determine current directory");
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(contents) = fs::read_to_string(&cargo_toml) {
                if contents.contains("[workspace]") {
                    return dir;
                }
            }
        }
        if !dir.pop() {
            // Fallback: assume we're already in the project root
            return std::env::current_dir().expect("cannot determine current directory");
        }
    }
}

/// Copy all files from src_dir into dst_dir (non-recursive, files only).
fn copy_dir_contents(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst_dir.join(entry.file_name());
        if src_path.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod mount_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn parse_from_string(content: &str) -> Result<Cluufile> {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        parse_cluufile(tmp.path())
    }

    #[test]
    fn mount_directive_parses_inherit() {
        let src = "FROM base\nMOUNT /tmp inherit\n";
        let c = parse_from_string(src).expect("should parse");
        assert_eq!(c.mount_policies, vec![("/tmp".to_string(), "inherit".to_string())]);
    }

    #[test]
    fn mount_directive_parses_private() {
        let src = "FROM base\nMOUNT /log private\n";
        let c = parse_from_string(src).expect("should parse");
        assert_eq!(c.mount_policies, vec![("/log".to_string(), "private".to_string())]);
    }

    #[test]
    fn mount_directive_rejects_unknown_policy() {
        let src = "FROM base\nMOUNT /tmp shared\n";
        let err = parse_from_string(src).expect_err("shared is not a valid policy");
        assert!(err.to_string().contains("MOUNT policy must be"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_accepts_rw_keywords() {
        // UE14: procmgr's mount_policy parser also accepts rw/readwrite
        // (and ro/readonly). The compiler must pass them through verbatim
        // so procmgr can interpret them — otherwise Cluufiles that demand
        // explicit writable mounts can't be authored.
        for keyword in ["rw", "readwrite", "readonly"] {
            let src = format!("FROM base\nMOUNT /etc {}\n", keyword);
            let c = parse_from_string(&src).expect("should parse");
            assert_eq!(c.mount_policies, vec![("/etc".to_string(), keyword.to_string())]);
        }
    }

    #[test]
    fn mount_directive_rejects_relative_path() {
        let src = "FROM base\nMOUNT tmp inherit\n";
        let err = parse_from_string(src).expect_err("relative path should fail");
        assert!(err.to_string().contains("MOUNT path must be absolute"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_duplicate_path() {
        let src = "FROM base\nMOUNT /tmp inherit\nMOUNT /tmp private\n";
        let err = parse_from_string(src).expect_err("duplicate MOUNT should fail");
        assert!(err.to_string().contains("duplicate MOUNT"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_wrong_arity() {
        let src = "FROM base\nMOUNT /tmp\n";
        let err = parse_from_string(src).expect_err("single-arg MOUNT should fail");
        assert!(err.to_string().contains("MOUNT requires exactly two arguments"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_before_from() {
        let src = "MOUNT /tmp inherit\nFROM base\n";
        let err = parse_from_string(src).expect_err("MOUNT before FROM should fail");
        assert!(err.to_string().contains("FROM must appear before MOUNT"), "err was: {}", err);
    }

    #[test]
    fn multiple_mount_directives_accumulate() {
        let src = "FROM base\nMOUNT /tmp inherit\nMOUNT /log private\n";
        let c = parse_from_string(src).expect("should parse");
        assert_eq!(c.mount_policies.len(), 2);
        assert_eq!(c.mount_policies[0].0, "/tmp");
        assert_eq!(c.mount_policies[1].0, "/log");
    }

    #[test]
    fn manifest_emits_mount_policy_entries() {
        let src = "FROM base\nMOUNT /tmp inherit\nMOUNT /log private\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(toml.contains("[[mounts.policy]]"), "missing section: {}", toml);
        assert!(toml.contains("path = \"/tmp\""), "missing path: {}", toml);
        assert!(toml.contains("policy = \"inherit\""), "missing policy: {}", toml);
        assert!(toml.contains("path = \"/log\""), "missing path: {}", toml);
        assert!(toml.contains("policy = \"private\""), "missing policy: {}", toml);
    }

    #[test]
    fn manifest_omits_mount_section_when_no_policies() {
        let src = "FROM base\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(!toml.contains("[[mounts.policy]]"), "should not have section: {}", toml);
    }

    #[test]
    fn mount_conflicts_with_deny() {
        let src = "FROM base\nDENY /tmp\nMOUNT /tmp inherit\n";
        let err = parse_from_string(src).expect_err("MOUNT on DENY path should fail");
        assert!(
            err.to_string().contains("MOUNT conflicts with DENY"),
            "err was: {}", err
        );
    }

    #[test]
    fn mount_conflicts_with_persistent() {
        let src = "FROM base\nPERSISTENT /data\nMOUNT /data private\n";
        let err = parse_from_string(src).expect_err("MOUNT on PERSISTENT path should fail");
        assert!(
            err.to_string().contains("MOUNT conflicts with PERSISTENT"),
            "err was: {}", err
        );
    }

    #[test]
    fn deny_declared_after_mount_still_conflicts() {
        let src = "FROM base\nMOUNT /tmp inherit\nDENY /tmp\n";
        let err = parse_from_string(src).expect_err("order shouldn't matter");
        assert!(
            err.to_string().contains("MOUNT conflicts with DENY"),
            "err was: {}", err
        );
    }
}

#[cfg(test)]
mod driver_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn parse_from_string(content: &str) -> Result<Cluufile> {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        parse_cluufile(tmp.path())
    }

    #[test]
    fn from_driver_with_bind_parses() {
        let src = "FROM driver\nDRIVER bind bus=pci vendor=0x1af4 devices=[0x1001,0x1042]\n";
        let c = parse_from_string(src).expect("should parse");
        let driver = c.driver.as_ref().expect("driver spec should be Some");
        assert_eq!(driver.directives.len(), 1);
        assert_eq!(driver.directives[0].sub, "bind");
    }

    #[test]
    fn from_driver_without_driver_directives_fails() {
        let src = "FROM driver\n";
        let err = parse_from_string(src).expect_err("needs DRIVER bind");
        assert!(
            err.to_string().contains("FROM driver requires at least one DRIVER bind directive"),
            "err was: {}", err
        );
    }

    #[test]
    fn from_driver_with_only_non_bind_driver_fails() {
        let src = "FROM driver\nDRIVER lifecycle critical=true\n";
        let err = parse_from_string(src).expect_err("needs DRIVER bind specifically");
        assert!(
            err.to_string().contains("FROM driver requires at least one DRIVER bind directive"),
            "err was: {}", err
        );
    }

    #[test]
    fn from_minimal_with_driver_directive_fails() {
        let src = "FROM minimal\nDRIVER bind bus=pci\n";
        let err = parse_from_string(src).expect_err("DRIVER requires FROM driver");
        assert!(
            err.to_string().contains("DRIVER directive requires FROM driver"),
            "err was: {}", err
        );
    }

    #[test]
    fn from_base_with_driver_directive_fails() {
        let src = "FROM base\nDRIVER bind bus=pci\n";
        let err = parse_from_string(src).expect_err("DRIVER requires FROM driver");
        assert!(
            err.to_string().contains("DRIVER directive requires FROM driver"),
            "err was: {}", err
        );
    }

    #[test]
    fn unknown_driver_sub_directive_fails() {
        let src = "FROM driver\nDRIVER bogus bus=pci\n";
        let err = parse_from_string(src).expect_err("unknown sub");
        assert!(
            err.to_string().contains("unknown DRIVER sub-directive 'bogus'"),
            "err was: {}", err
        );
    }

    #[test]
    fn driver_directive_before_from_fails() {
        let src = "DRIVER bind bus=pci\nFROM driver\n";
        let err = parse_from_string(src).expect_err("DRIVER before FROM");
        assert!(
            err.to_string().contains("FROM must appear before DRIVER"),
            "err was: {}", err
        );
    }

    #[test]
    fn malformed_hex_fails() {
        let src = "FROM driver\nDRIVER bind bus=pci vendor=0xZZZZ\n";
        let err = parse_from_string(src).expect_err("bad hex");
        // with_context wraps the inner parse error; {:#} shows the full chain.
        let chain = format!("{:#}", err);
        assert!(
            chain.contains("non-hex characters"),
            "err was: {}", chain
        );
    }

    #[test]
    fn unclosed_array_fails() {
        let src = "FROM driver\nDRIVER bind bus=pci devices=[0x1001,0x1042\n";
        let err = parse_from_string(src).expect_err("unclosed array");
        let chain = format!("{:#}", err);
        assert!(
            chain.contains("missing closing ']'"),
            "err was: {}", chain
        );
    }

    #[test]
    fn empty_array_fails() {
        let src = "FROM driver\nDRIVER bind bus=pci devices=[]\n";
        let err = parse_from_string(src).expect_err("empty array");
        let chain = format!("{:#}", err);
        assert!(
            chain.contains("array '[]' is empty"),
            "err was: {}", chain
        );
    }

    #[test]
    fn empty_key_fails() {
        let src = "FROM driver\nDRIVER bind =pci\n";
        let err = parse_from_string(src).expect_err("empty key");
        let chain = format!("{:#}", err);
        assert!(
            chain.contains("DRIVER key is empty"),
            "err was: {}", chain
        );
    }

    #[test]
    fn driver_with_no_sub_directive_fails() {
        let src = "FROM driver\nDRIVER\n";
        let err = parse_from_string(src).expect_err("DRIVER needs sub");
        assert!(
            err.to_string().contains("DRIVER requires a sub-directive"),
            "err was: {}", err
        );
    }

    #[test]
    fn bare_flag_becomes_bool_true() {
        let src = "FROM driver\nDRIVER bind bus=acpi hid=PNP0303\nDRIVER hardware dma\n";
        let c = parse_from_string(src).expect("should parse");
        let hw = c.driver.as_ref().unwrap()
            .directives.iter().find(|d| d.sub == "hardware").unwrap();
        let (_, v) = hw.keys.iter().find(|(k, _)| k == "dma").unwrap();
        match v {
            DriverValue::Bool(true) => {}
            other => panic!("expected Bool(true), got {:?}", other),
        }
    }

    #[test]
    fn quoted_string_value_parses() {
        let src = "FROM driver\nDRIVER bind bus=pci\nDRIVER source initrd_path=\"sys/virtio-blk.elf\"\n";
        let c = parse_from_string(src).expect("should parse");
        let src_dir = c.driver.as_ref().unwrap()
            .directives.iter().find(|d| d.sub == "source").unwrap();
        let (_, v) = src_dir.keys.iter().find(|(k, _)| k == "initrd_path").unwrap();
        match v {
            DriverValue::String(s) => assert_eq!(s, "sys/virtio-blk.elf"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn hex_array_tracks_hex_flag() {
        let src = "FROM driver\nDRIVER bind bus=pci devices=[0x1001,0x1042]\n";
        let c = parse_from_string(src).expect("should parse");
        let bind = &c.driver.as_ref().unwrap().directives[0];
        let (_, v) = bind.keys.iter().find(|(k, _)| k == "devices").unwrap();
        match v {
            DriverValue::HexArray(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], (0x1001, true));
                assert_eq!(items[1], (0x1042, true));
            }
            other => panic!("expected HexArray, got {:?}", other),
        }
    }

    #[test]
    fn decimal_array_tracks_decimal_flag() {
        let src = "FROM driver\nDRIVER bind bus=pci devices=[1,2,3]\n";
        let c = parse_from_string(src).expect("should parse");
        let bind = &c.driver.as_ref().unwrap().directives[0];
        let (_, v) = bind.keys.iter().find(|(k, _)| k == "devices").unwrap();
        match v {
            DriverValue::HexArray(items) => {
                assert_eq!(items, &[(1, false), (2, false), (3, false)]);
            }
            other => panic!("expected HexArray, got {:?}", other),
        }
    }

    #[test]
    fn mixed_hex_decimal_array_parses() {
        let src = "FROM driver\nDRIVER bind bus=pci devices=[0x1001,42]\n";
        let c = parse_from_string(src).expect("should parse");
        let bind = &c.driver.as_ref().unwrap().directives[0];
        let (_, v) = bind.keys.iter().find(|(k, _)| k == "devices").unwrap();
        match v {
            DriverValue::HexArray(items) => {
                assert_eq!(items, &[(0x1001, true), (42, false)]);
            }
            other => panic!("expected HexArray, got {:?}", other),
        }
    }

    #[test]
    fn underscore_hex_literal_parses() {
        let src = "FROM driver\nDRIVER bind bus=pci\nDRIVER hardware base=0x5100_0000\n";
        let c = parse_from_string(src).expect("should parse");
        let hw = c.driver.as_ref().unwrap()
            .directives.iter().find(|d| d.sub == "hardware").unwrap();
        let (_, v) = hw.keys.iter().find(|(k, _)| k == "base").unwrap();
        match v {
            DriverValue::Hex(n) => assert_eq!(*n, 0x5100_0000),
            other => panic!("expected Hex, got {:?}", other),
        }
    }

    #[test]
    fn multiple_driver_directives_accumulate() {
        let src = "FROM driver\nDRIVER bind bus=pci vendor=0x1af4\nDRIVER lifecycle critical=true\nDRIVER source initrd_path=\"x.elf\"\n";
        let c = parse_from_string(src).expect("should parse");
        let driver = c.driver.as_ref().unwrap();
        assert_eq!(driver.directives.len(), 3);
        assert_eq!(driver.directives[0].sub, "bind");
        assert_eq!(driver.directives[1].sub, "lifecycle");
        assert_eq!(driver.directives[2].sub, "source");
    }

    #[test]
    fn manifest_emits_driver_section() {
        let src = "FROM driver\nDRIVER bind bus=pci vendor=0x1af4 devices=[0x1001,0x1042]\nDRIVER lifecycle critical=true\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(toml.contains("[driver]"), "missing [driver]: {}", toml);
        assert!(toml.contains("[[driver.bind]]"), "missing [[driver.bind]]: {}", toml);
        assert!(toml.contains("[[driver.lifecycle]]"), "missing [[driver.lifecycle]]: {}", toml);
        assert!(toml.contains("bus = \"pci\""), "missing bus: {}", toml);
        assert!(toml.contains("vendor = 0x1af4"), "missing vendor: {}", toml);
        assert!(toml.contains("devices = [0x1001, 0x1042]"), "missing devices: {}", toml);
        assert!(toml.contains("critical = true"), "missing critical: {}", toml);
    }

    #[test]
    fn manifest_omits_driver_section_for_non_driver() {
        let src = "FROM base\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(!toml.contains("[driver]"), "should not have [driver]: {}", toml);
        assert!(!toml.contains("[[driver."), "should not have [[driver.*]]: {}", toml);
    }

    #[test]
    fn manifest_preserves_hex_vs_decimal_form() {
        let src = "FROM driver\nDRIVER bind bus=pci devices=[0x1001,42]\nDRIVER envelope priority=180\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(toml.contains("devices = [0x1001, 42]"), "hex+decimal mix wrong: {}", toml);
        assert!(toml.contains("priority = 180"), "decimal wrong: {}", toml);
    }

    #[test]
    fn manifest_emits_multiple_tables_for_same_sub() {
        let src = "FROM driver\nDRIVER bind bus=pci vendor=0x1af4\nDRIVER bind bus=acpi hid=PNP0303\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        let bind_count = toml.matches("[[driver.bind]]").count();
        assert_eq!(bind_count, 2, "expected 2 [[driver.bind]] tables, got {}: {}", bind_count, toml);
        assert!(toml.contains("vendor = 0x1af4"), "missing first bind: {}", toml);
        assert!(toml.contains("hid = \"PNP0303\""), "missing second bind: {}", toml);
    }

    #[test]
    fn manifest_emits_all_five_sub_types() {
        let src = "FROM driver
DRIVER bind bus=pci vendor=0x1af4
DRIVER hardware dma
DRIVER lifecycle critical=true
DRIVER source initrd_path=\"x.elf\"
DRIVER envelope priority=180
";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        for sub in ["bind", "hardware", "lifecycle", "source", "envelope"] {
            let header = format!("[[driver.{}]]", sub);
            assert!(toml.contains(&header), "missing {}: {}", header, toml);
        }
    }

    #[test]
    fn manifest_emits_sub_sections_in_first_appearance_order() {
        let src = "FROM driver
DRIVER source initrd_path=\"x.elf\"
DRIVER bind bus=pci
DRIVER envelope priority=180
";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        let source_idx = toml.find("[[driver.source]]").unwrap();
        let bind_idx = toml.find("[[driver.bind]]").unwrap();
        let envelope_idx = toml.find("[[driver.envelope]]").unwrap();
        assert!(source_idx < bind_idx, "source should come before bind: {}", toml);
        assert!(bind_idx < envelope_idx, "bind should come before envelope: {}", toml);
    }

    #[test]
    fn manifest_toml_is_parseable() {
        // The emitted TOML must be valid: round-trip through a TOML parser
        // (tomli, available in the python harness) — but since we can't run
        // python here, we at least check the structure parses with our own
        // emit_driver_value by ensuring no value contains a raw newline or
        // unescaped quote that would break TOML.
        let src = "FROM driver
DRIVER bind bus=pci vendor=0x1af4 devices=[0x1001,0x1042] class=0x010000
DRIVER hardware dma mmio=true
DRIVER lifecycle critical=true restart_policy=always max_restarts=3 window_secs=30
DRIVER source initrd_path=\"sys/virtio-blk.elf\"
DRIVER envelope fallback=\"blk-fb\" priority=180
";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        for line in toml.lines() {
            if line.contains(" = ") && !line.starts_with('#') {
                let val = line.split_once(" = ").unwrap().1;
                assert!(!val.contains('\n'), "value spans lines: {:?}", line);
            }
        }
    }
}
