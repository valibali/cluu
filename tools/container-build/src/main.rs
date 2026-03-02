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
    deny_inherit: bool,
    deny: Vec<String>,
    detach: bool,
    restart_policy: Option<(String, Option<usize>, Option<u64>)>,
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
    let mut deny_inherit = false;
    let mut deny: Vec<String> = Vec::new();
    let mut detach = false;
    let mut restart_policy: Option<(String, Option<usize>, Option<u64>)> = None;
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
                match rest {
                    "irq" | "framebuffer" => {
                        if devices.contains(&rest.to_string()) {
                            bail!("{}:{}: duplicate DEVICE '{}'", path.display(), lineno, rest);
                        }
                        devices.push(rest.to_string());
                    }
                    _ => {
                        bail!(
                            "{}:{}: DEVICE must be 'irq' or 'framebuffer', got '{}'",
                            path.display(), lineno, rest
                        );
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
            unknown => {
                bail!(
                    "{}:{}: unknown directive '{}'",
                    path.display(), lineno, unknown
                );
            }
        }

        saw_directive = true;
    }

    let base = base.ok_or_else(|| {
        anyhow::anyhow!("{}: missing required FROM directive", path.display())
    })?;

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
        deny_inherit,
        deny,
        detach,
        restart_policy,
    })
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
    if !cluufile.devices.is_empty() {
        out.push_str("\n[hardware]\ndevices = [");
        for (i, dev) in cluufile.devices.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("\"{}\"", dev));
        }
        out.push_str("]\n");
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

    // [mounts] — only if deny_inherit or deny paths specified
    if cluufile.deny_inherit || !cluufile.deny.is_empty() {
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
    }

    out
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
    println!("  BUILD \"{}\"", step.command);

    // Container-scoped build output directory to avoid parallel build races.
    let build_dir = project_root
        .join("target/containers")
        .join(container_name)
        .join("build");
    fs::create_dir_all(&build_dir)?;

    let status = Command::new("sh")
        .arg("-c")
        .arg(&step.command)
        .current_dir(project_root)
        .env("CLUU_BUILD_OUTPUT_DIR", &build_dir)
        .status()
        .with_context(|| format!("Failed to execute BUILD command: {}", step.command))?;

    if !status.success() {
        bail!(
            "BUILD command failed (exit {}): {}",
            status.code().unwrap_or(-1),
            step.command
        );
    }

    // Look for the build output: first in the container-scoped dir, then the
    // original path.  The scoped dir is preferred because build tools that
    // honour CLUU_BUILD_OUTPUT_DIR write there, making parallel builds safe.
    let filename = Path::new(&step.build_output)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| step.build_output.clone());
    let scoped_src = build_dir.join(&filename);
    let original_src = project_root.join(&step.build_output);

    let src = if scoped_src.exists() {
        scoped_src
    } else if original_src.exists() {
        original_src
    } else {
        bail!(
            "BUILD output not found after command: {} (checked {} and {})",
            step.build_output,
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

    println!("  BUILD {} → {}", step.build_output, step.container_path);
    Ok(())
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
