//! Build orchestration for CLUU
//!
//! Usage:
//!   cargo xtask build          # Build everything
//!   cargo xtask run            # Run existing disk image in QEMU
//!   cargo xtask run --build    # Build and then run in QEMU
//!   cargo xtask test           # Run all tests
//!   cargo xtask clean          # Clean all build artifacts

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLUU_TARGET_TRIPLET: &str = "x86_64-cluu";
const NEWLIB_TARGET_TRIPLET: &str = "x86_64-unknown-elf";
const NEWLIB_CLUU_TRIPLET: &str = "x86_64-cluu-elf";
const CLUU_CLANG_TARGET: &str = "x86_64-unknown-none-elf";
const DEFAULT_NEWLIB_VERSION: &str = "4.4.0.20231231";
const DEFAULT_MICROPYTHON_VERSION: &str = "v1.22.0";
const DEFAULT_MICROPYTHON_REF: &str = "v1.22.0";
const EXTERNAL_SOURCES_CONFIG_REL: &str = "external/sources.env";
const BOOT_MANIFEST_HMAC_KEY: [u8; 32] = [
    0x43, 0x4c, 0x55, 0x55, 0x2d, 0x42, 0x4f, 0x4f, 0x54, 0x2d, 0x4d, 0x41, 0x4e, 0x49, 0x46, 0x45,
    0x53, 0x54, 0x2d, 0x4b, 0x45, 0x59, 0x2d, 0x30, 0x31, 0x2d, 0x44, 0x45, 0x56, 0x2d, 0x41, 0x31,
];

const RIGHT_READ: u32 = 1 << 0;
const RIGHT_WRITE: u32 = 1 << 1;
const RIGHT_EXECUTE: u32 = 1 << 2;
const RIGHT_CREATE: u32 = 1 << 3;
const RIGHT_DESTROY: u32 = 1 << 4;
const RIGHT_GRANT: u32 = 1 << 5;
const RIGHT_MAP: u32 = 1 << 6;
const RIGHT_MANAGE: u32 = 1 << 7;
const RIGHT_THREAD_CONTROL: u32 = 1 << 8;
const RIGHT_THREAD_SUSPEND: u32 = 1 << 9;
const RIGHT_SPACE_MAP: u32 = 1 << 16;
const RIGHT_SPACE_UNMAP: u32 = 1 << 17;
const RIGHT_SPACE_GRANT: u32 = 1 << 18;
const RIGHT_IPC_SEND: u32 = 1 << 24;
const RIGHT_IPC_RECV: u32 = 1 << 25;
const RIGHT_IPC_CALL: u32 = 1 << 26;
const RIGHT_IRQ_HANDLE: u32 = 1 << 28;
const RIGHT_IRQ_ACK: u32 = 1 << 29;
const RIGHT_PCI_ACCESS: u32 = 1 << 30;

const ALL_RIGHTS_MASK: u32 = RIGHT_READ
    | RIGHT_WRITE
    | RIGHT_EXECUTE
    | RIGHT_CREATE
    | RIGHT_DESTROY
    | RIGHT_GRANT
    | RIGHT_MAP
    | RIGHT_MANAGE
    | RIGHT_THREAD_CONTROL
    | RIGHT_THREAD_SUSPEND
    | RIGHT_SPACE_MAP
    | RIGHT_SPACE_UNMAP
    | RIGHT_SPACE_GRANT
    | RIGHT_IPC_SEND
    | RIGHT_IPC_RECV
    | RIGHT_IPC_CALL
    | RIGHT_IRQ_HANDLE
    | RIGHT_IRQ_ACK
    | RIGHT_PCI_ACCESS;

#[derive(Debug, Clone)]
struct ExternalSourcesConfig {
    config_path: PathBuf,
    newlib_version: String,
    newlib_url: String,
    newlib_dir: PathBuf,
    newlib_patch_files: Vec<String>,
    micropython_version: String,
    micropython_repo: String,
    micropython_ref: Option<String>,
    micropython_dir: PathBuf,
    micropython_patch_files: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BuildUi {
    Linear,
    Rich,
}

#[derive(Clone, Debug)]
struct RichTask {
    name: String,
    args: Vec<String>,
    deps: Vec<String>,
}

#[derive(Parser)]
#[command(name = "xtask", about = "CLUU build system")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build everything (kernel + userspace + disk image)
    Build {
        #[arg(long, default_value = "dev")]
        profile: String,
        /// Build UI mode: linear output or rich progress view with per-task logs
        #[arg(long, value_enum, default_value_t = BuildUi::Rich)]
        ui: BuildUi,
    },
    /// Run existing disk image in QEMU
    Run {
        /// Build first, then run QEMU
        #[arg(long)]
        build: bool,
        #[arg(long, default_value = "dev")]
        profile: String,
        /// Enable debug mode (pause for GDB, telnet serial on 4321)
        #[arg(long)]
        debug: bool,
        /// Build UI mode when used with --build
        #[arg(long, value_enum, default_value_t = BuildUi::Rich)]
        ui: BuildUi,
    },
    /// Run all tests
    Test,
    /// Clean all build artifacts
    Clean,
    /// Clean all generated artifacts, including toolchain outputs and staging dirs
    CleanFull,
    /// Full deterministic rebuild from scratch (newlib + syscalls + crt0 + all images)
    RebuildFull {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Verify host tools and key build artifacts
    Doctor,
    /// View/tail rich build logs under target/logs
    Logs {
        /// Specific rich-build run id (timestamp directory) or path; defaults to latest run
        #[arg(long)]
        run: Option<String>,
        /// Task log name (for example: userspace, kernel, c-programs, micropython, initrd)
        #[arg(long)]
        task: Option<String>,
        /// Number of lines to show from the end of the log
        #[arg(long, default_value_t = 80)]
        lines: usize,
        /// Keep streaming appended log output (tail -f behavior)
        #[arg(long)]
        follow: bool,
    },
    /// Build only userspace programs
    Userspace {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Build only kernel
    Kernel {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Build newlib C library for CLUU
    BuildNewlib,
    /// Build libcluu_syscalls static library
    BuildSyscalls {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Assemble crt0.o for C programs
    BuildCrt0,
    /// Build a C program
    BuildC {
        /// Name for the output binary
        name: String,
        /// Path to the C source file
        source: PathBuf,
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Show sysroot path
    Sysroot,
    /// Setup complete C toolchain (newlib + syscalls + crt0)
    SetupC,
    /// Run QEMU harness matrix for churn/leak/failpoint regressions
    HarnessMatrix {
        /// Reuse existing build artifacts for all matrix cases
        #[arg(long)]
        no_build: bool,
    },
    /// Run repeated fairness SLO sweep and collect summary metrics
    HarnessSlo {
        /// Reuse existing build artifacts for all runs
        #[arg(long)]
        no_build: bool,
        /// Number of fairness runs to execute
        #[arg(long, default_value_t = 5)]
        repeats: u32,
    },
    /// Internal: build all C programs
    #[command(hide = true)]
    BuildCPrograms {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Internal: build micropython port
    #[command(hide = true)]
    BuildMicropython,
    /// Internal: create initrd
    #[command(hide = true)]
    CreateInitrd {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Internal: create userspace block image
    #[command(hide = true)]
    CreateUserBlockImage {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Internal: create boot disk image
    #[command(hide = true)]
    CreateDiskImage {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Build a container image from a Cluufile
    ContainerBuild {
        /// Path to the Cluufile
        path: PathBuf,
    },
    /// Internal: build all container images from containers/*/Cluufile
    #[command(hide = true)]
    BuildContainers,
    /// Internal: build klibcluu kernel library
    #[command(hide = true)]
    BuildKlibcluu,
    /// Internal: build libcluu userspace library
    #[command(hide = true)]
    BuildLibcluu {
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Internal: build a single init primordial crate
    #[command(hide = true)]
    BuildInitCrate {
        name: String,
        #[arg(long, default_value = "dev")]
        profile: String,
    },
    /// Internal: build a single container by name
    #[command(hide = true)]
    BuildSingleContainer { name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { profile, ui } => {
            build_pipeline(&profile, ui)?;
            println!("✓ Build complete: target/cluu.img");
        }
        Commands::Run {
            build,
            profile,
            debug,
            ui,
        } => {
            if build {
                build_pipeline(&profile, ui)?;
            }
            run_qemu(debug)?;
        }
        Commands::Test => {
            run_tests()?;
        }
        Commands::Clean => {
            clean()?;
        }
        Commands::CleanFull => {
            clean_full()?;
        }
        Commands::RebuildFull { profile } => {
            rebuild_full(&profile)?;
        }
        Commands::Doctor => {
            doctor()?;
        }
        Commands::Logs {
            run,
            task,
            lines,
            follow,
        } => {
            view_logs(run.as_deref(), task.as_deref(), lines, follow)?;
        }
        Commands::Userspace { profile } => {
            build_userspace(&profile)?;
        }
        Commands::Kernel { profile } => {
            build_kernel(&profile)?;
        }
        Commands::BuildNewlib => {
            build_newlib()?;
        }
        Commands::BuildSyscalls { profile } => {
            build_syscalls(&profile)?;
        }
        Commands::BuildCrt0 => {
            build_crt0()?;
        }
        Commands::BuildC {
            name,
            source,
            profile,
        } => {
            build_c_program(&name, &source, &profile)?;
        }
        Commands::Sysroot => {
            println!("{}", sysroot_path().display());
        }
        Commands::SetupC => {
            setup_c_toolchain()?;
        }
        Commands::HarnessMatrix { no_build } => {
            run_harness_matrix(no_build)?;
        }
        Commands::HarnessSlo { no_build, repeats } => {
            run_harness_slo(no_build, repeats)?;
        }
        Commands::BuildCPrograms { profile } => {
            build_c_programs(&profile)?;
        }
        Commands::BuildMicropython => {
            build_micropython()?;
        }
        Commands::CreateInitrd { profile } => {
            create_initrd(&profile)?;
        }
        Commands::CreateUserBlockImage { profile } => {
            create_user_block_image(&profile)?;
        }
        Commands::CreateDiskImage { profile } => {
            create_disk_image(&profile)?;
        }
        Commands::ContainerBuild { path } => {
            // Delegate to standalone container-build tool
            let status = Command::new("cargo")
                .args(["run", "-p", "container-build", "--"])
                .arg(&path)
                .status()
                .context("Failed to run container-build tool")?;
            if !status.success() {
                bail!("container-build failed with exit code {:?}", status.code());
            }
        }
        Commands::BuildContainers => {
            build_containers()?;
        }
        Commands::BuildKlibcluu => {
            build_klibcluu()?;
        }
        Commands::BuildLibcluu { profile } => {
            build_libcluu(&profile)?;
        }
        Commands::BuildInitCrate { name, profile } => {
            build_init_crate(&name, &profile)?;
        }
        Commands::BuildSingleContainer { name } => {
            build_single_container(&name)?;
        }
    }

    Ok(())
}

fn build_pipeline(profile: &str, ui: BuildUi) -> Result<()> {
    match ui {
        BuildUi::Linear => build_pipeline_linear(profile),
        BuildUi::Rich => build_pipeline_rich(profile),
    }
}

fn build_pipeline_linear(profile: &str) -> Result<()> {
    // Dependencies
    build_klibcluu()?;
    build_libcluu(profile)?;
    build_newlib()?;
    build_syscalls(profile)?;
    build_crt0()?;
    // Kernel
    build_kernel(profile)?;
    // Init primordials
    for name in INIT_CRATES {
        build_init_crate(name, profile)?;
    }
    // Containers
    build_containers()?;
    // Packaging
    create_initrd(profile)?;
    create_user_block_image(profile)?;
    create_disk_image(profile)?;
    Ok(())
}

fn logs_root_dir() -> PathBuf {
    project_root().join("target").join("logs")
}

fn work_units_from_line(line: &str) -> u32 {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed == "(output)" {
        return 0;
    }
    if trimmed.contains("Blocking waiting for file lock") {
        return 0;
    }

    let lower = trimmed.to_ascii_lowercase();

    // Strong build-step indicators.
    if lower.starts_with("compiling ")
        || lower.starts_with("linking ")
        || lower.starts_with("finished ")
        || lower.contains("building c object")
        || lower.contains("building cxx object")
        || lower.contains("generating ")
        || lower.contains("assembling ")
    {
        return 1;
    }

    // File-oriented heuristic for tool outputs that don't use the cargo-style prefixes.
    let file_markers = [".rs", ".c", ".h", ".s", ".asm", ".o", ".a", ".ld"];
    if file_markers.iter().any(|marker| lower.contains(marker)) {
        return 1;
    }

    0
}

fn is_work_unit_line(line: &str) -> bool {
    work_units_from_line(line) > 0
}

fn count_work_units_in_log(log_path: &Path) -> u32 {
    let Ok(content) = fs::read_to_string(log_path) else {
        return 0;
    };
    content.lines().fold(0u32, |acc, line| {
        acc.saturating_add(work_units_from_line(line))
    })
}

fn default_expected_work_units(task_id: &str) -> u32 {
    match task_id {
        "dep-klibcluu" => 60,
        "dep-libcluu" => 80,
        "dep-newlib" => 320,
        "dep-syscalls" => 40,
        "dep-crt0" => 8,
        id if id.starts_with("init-") => 40,
        "kernel" => 120,
        "initrd" => 24,
        "userdisk" => 48,
        "disk-image" => 8,
        id if id.starts_with("container-") => 20,
        _ => 80,
    }
}

fn historical_expected_work_units(task_id: &str, logs_root: &Path) -> u32 {
    if !logs_root.exists() {
        return default_expected_work_units(task_id);
    }

    let mut runs: Vec<PathBuf> = match fs::read_dir(logs_root) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    };
    runs.sort_by_key(|path| path.file_name().map(|n| n.to_os_string()));
    runs.reverse();

    let mut samples: Vec<u32> = Vec::new();
    for run_dir in runs.into_iter().take(10) {
        let path = run_dir.join(format!("{task_id}.log"));
        if !path.exists() {
            continue;
        }
        let count = count_work_units_in_log(&path);
        if count > 0 {
            samples.push(count);
        }
    }

    if samples.is_empty() {
        return default_expected_work_units(task_id);
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn select_log_run_dir(run: Option<&str>) -> Result<PathBuf> {
    let root = logs_root_dir();
    let legacy_root = project_root().join("target").join("xtask-logs");
    if let Some(run_arg) = run {
        let provided = PathBuf::from(run_arg);
        let candidates = if provided.is_absolute() {
            vec![provided]
        } else {
            vec![
                root.join(run_arg),
                legacy_root.join(run_arg),
                project_root().join(run_arg),
            ]
        };
        for candidate in candidates {
            if candidate.is_dir() {
                return Ok(candidate);
            }
        }
        bail!(
            "Log run '{}' not found. Expected a directory under {}",
            run_arg,
            root.display()
        );
    }

    if !root.exists() && !legacy_root.exists() {
        bail!(
            "No rich-build logs found yet (missing {}). Run a rich build first: cargo xtask build --ui rich",
            root.display()
        );
    }

    let mut runs: Vec<PathBuf> = Vec::new();
    if root.exists() {
        runs.extend(
            fs::read_dir(&root)
                .with_context(|| format!("Failed to read {}", root.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    if legacy_root.exists() {
        runs.extend(
            fs::read_dir(&legacy_root)
                .with_context(|| format!("Failed to read {}", legacy_root.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    runs.sort_by_key(|path| path.file_name().map(|n| n.to_os_string()));

    runs.pop().with_context(|| {
        format!(
            "No rich-build runs found in {}. Run: cargo xtask build --ui rich",
            root.display()
        )
    })
}

fn list_task_logs(run_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut logs: Vec<PathBuf> = fs::read_dir(run_dir)
        .with_context(|| format!("Failed to read {}", run_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "log")
                .unwrap_or(false)
        })
        .collect();
    logs.sort();
    Ok(logs)
}

fn resolve_task_log(run_dir: &Path, task: &str) -> Result<PathBuf> {
    let file_name = if task.ends_with(".log") {
        task.to_string()
    } else {
        format!("{task}.log")
    };
    let log_path = run_dir.join(file_name);
    if log_path.exists() {
        return Ok(log_path);
    }

    let available = list_task_logs(run_dir)?
        .into_iter()
        .filter_map(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .collect::<Vec<String>>()
        .join(", ");
    bail!(
        "Task log '{}' not found in {}. Available: {}",
        task,
        run_dir.display(),
        available
    );
}

fn print_log_tail(log_path: &Path, lines: usize) {
    let tail = read_log_tail(log_path, lines);
    if tail.is_empty() {
        println!("(log is currently empty)");
        return;
    }
    print!("{tail}");
    if !tail.ends_with('\n') {
        println!();
    }
}

fn follow_log(log_path: &Path) -> Result<()> {
    let mut offset = fs::metadata(log_path)
        .with_context(|| format!("Failed to stat {}", log_path.display()))?
        .len();

    loop {
        if !log_path.exists() {
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        let file_size = fs::metadata(log_path)
            .with_context(|| format!("Failed to stat {}", log_path.display()))?
            .len();
        if file_size < offset {
            offset = 0;
        }

        let mut file = File::open(log_path)
            .with_context(|| format!("Failed to open {}", log_path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("Failed to seek {}", log_path.display()))?;

        let mut new_data = String::new();
        file.read_to_string(&mut new_data)
            .with_context(|| format!("Failed to read {}", log_path.display()))?;

        if !new_data.is_empty() {
            offset += new_data.len() as u64;
            print!("{new_data}");
            io::stdout().flush().context("Failed to flush stdout")?;
        }

        thread::sleep(Duration::from_millis(200));
    }
}

fn view_logs(run: Option<&str>, task: Option<&str>, lines: usize, follow: bool) -> Result<()> {
    let run_dir = select_log_run_dir(run)?;
    println!("▸ Logs run: {}", run_dir.display());

    if task.is_none() {
        if follow {
            bail!("--follow requires --task <name>");
        }

        let logs = list_task_logs(&run_dir)?;
        if logs.is_empty() {
            println!("No task logs found in this run directory.");
            return Ok(());
        }

        println!("Available task logs:");
        for log in logs {
            let name = log
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            println!("  {name}");
        }
        println!();
        println!("Use: cargo xtask logs --task <name> [--lines N] [--follow]");
        return Ok(());
    }

    let task_name = task.unwrap();
    let log_path = resolve_task_log(&run_dir, task_name)?;
    println!("▸ Task log: {}", log_path.display());
    println!();
    print_log_tail(&log_path, lines);

    if follow {
        println!();
        println!("--- following (Ctrl+C to stop) ---");
        follow_log(&log_path)?;
    }

    Ok(())
}

fn rich_log_dir() -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock before UNIX_EPOCH")?
        .as_secs();
    let dir = project_root()
        .join("target")
        .join("logs")
        .join(stamp.to_string());
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn sanitize_live_line(raw: &str) -> String {
    // Strip ANSI/control noise so pane lines stay stable.
    let mut out = String::with_capacity(raw.len());
    let mut in_escape = false;

    for ch in raw.chars() {
        if in_escape {
            // End of CSI/escape sequence.
            if ('@'..='~').contains(&ch) {
                in_escape = false;
            }
            continue;
        }

        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        if ch == '\n' || ch == '\r' {
            continue;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }

    let trimmed = out.trim();
    if trimmed.is_empty() {
        "(output)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn truncate_live_line(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut out: String = line.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn ansi_escape_len(bytes: &[u8], start: usize) -> usize {
    if start >= bytes.len() || bytes[start] != 0x1b {
        return 0;
    }
    if start + 1 >= bytes.len() {
        return 1;
    }
    match bytes[start + 1] {
        b'[' => {
            // CSI: ESC [ ... <final-byte 0x40..0x7E>
            let mut i = start + 2;
            while i < bytes.len() {
                if (0x40..=0x7e).contains(&bytes[i]) {
                    return i - start + 1;
                }
                i += 1;
            }
            bytes.len() - start
        }
        b']' => {
            // OSC: ESC ] ... BEL or ST(ESC \)
            let mut i = start + 2;
            while i < bytes.len() {
                if bytes[i] == 0x07 {
                    return i - start + 1;
                }
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i - start + 2;
                }
                i += 1;
            }
            bytes.len() - start
        }
        _ => {
            // Other ESC sequence, assume 2-byte sequence.
            (start + 2).min(bytes.len()) - start
        }
    }
}

fn visible_width_ansi(line: &str) -> usize {
    let mut width = 0usize;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let esc_len = ansi_escape_len(bytes, i).max(1);
            i = i.saturating_add(esc_len);
            continue;
        }
        if let Some(ch) = line[i..].chars().next() {
            i += ch.len_utf8();
            width = width.saturating_add(1);
        } else {
            break;
        }
    }
    width
}

fn take_visible_ansi(line: &str, visible_limit: usize) -> String {
    let mut out = String::new();
    let mut visible = 0usize;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let esc_len = ansi_escape_len(bytes, i).max(1);
            let end = (i + esc_len).min(bytes.len());
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }
        if visible >= visible_limit {
            break;
        }
        if let Some(ch) = line[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
            visible = visible.saturating_add(1);
        } else {
            break;
        }
    }

    out
}

fn clamp_render_line(line: &str, width: usize, use_color: bool) -> String {
    let width = width.max(1);
    if !use_color {
        return truncate_live_line(line, width);
    }
    if visible_width_ansi(line) <= width {
        return line.to_string();
    }

    let mut out = take_visible_ansi(line, width.saturating_sub(1));
    out.push('…');
    out.push_str("\x1b[0m");
    out
}

fn fit_render_line(line: &str, width: usize, use_color: bool) -> String {
    let width = width.max(1);
    let clamped = clamp_render_line(line, width, use_color);
    let visible = if use_color {
        visible_width_ansi(&clamped)
    } else {
        clamped.chars().count()
    };
    if visible >= width {
        return clamped;
    }
    let mut out = clamped;
    out.push_str(&" ".repeat(width - visible));
    out
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug)]
struct RichTreeNode {
    id: String,
    label: String,
    parent: Option<String>,
    children: Vec<String>,
    is_leaf: bool,
    status: NodeStatus,
    last_line: String,
    fail_log: Option<String>,
    progress: f32,
    work_units_seen: u32,
    work_units_expected: u32,
    start_tick: u64,
}

#[derive(Clone, Debug)]
struct RichTreeNodeDef {
    id: String,
    label: String,
    parent: Option<String>,
    is_leaf: bool,
}

#[derive(Clone, Debug)]
struct RichTreeSnapshot {
    title: String,
    logs_dir: String,
    order: Vec<String>,
    nodes: HashMap<String, RichTreeNode>,
    stop: bool,
}

#[derive(Debug)]
struct RichTreeState {
    title: String,
    logs_dir: PathBuf,
    order: Vec<String>,
    nodes: HashMap<String, RichTreeNode>,
    tick: u64,
    stop: bool,
}

#[derive(Clone)]
struct RichTreeUi {
    state: Arc<Mutex<RichTreeState>>,
}

impl RichTreeUi {
    fn new(title: String, logs_dir: PathBuf, defs: &[RichTreeNodeDef]) -> Self {
        let logs_root = logs_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(logs_root_dir);
        let mut nodes: HashMap<String, RichTreeNode> = HashMap::new();
        for def in defs {
            let expected = if def.is_leaf {
                historical_expected_work_units(&def.id, &logs_root)
            } else {
                0
            };
            nodes.insert(
                def.id.clone(),
                RichTreeNode {
                    id: def.id.clone(),
                    label: def.label.clone(),
                    parent: def.parent.clone(),
                    children: Vec::new(),
                    is_leaf: def.is_leaf,
                    status: NodeStatus::Pending,
                    last_line: String::new(),
                    fail_log: None,
                    progress: 0.0,
                    work_units_seen: 0,
                    work_units_expected: expected,
                    start_tick: 0,
                },
            );
        }
        for def in defs {
            if let Some(parent_id) = def.parent.as_deref() {
                if let Some(parent) = nodes.get_mut(parent_id) {
                    parent.children.push(def.id.clone());
                }
            }
        }

        let order = compute_tree_order(defs, &nodes);
        Self {
            state: Arc::new(Mutex::new(RichTreeState {
                title,
                logs_dir,
                order,
                nodes,
                tick: 0,
                stop: false,
            })),
        }
    }

    fn snapshot(&self) -> RichTreeSnapshot {
        let state = self.state.lock().expect("tree state lock poisoned");
        RichTreeSnapshot {
            title: state.title.clone(),
            logs_dir: state.logs_dir.display().to_string(),
            order: state.order.clone(),
            nodes: state.nodes.clone(),
            stop: state.stop,
        }
    }

    fn start_renderer(&self) -> thread::JoinHandle<()> {
        let ui = self.clone();
        thread::spawn(move || {
            let use_ansi = should_use_ansi_controls();
            if use_ansi {
                // Hidden cursor + no line-wrap to prevent redraw artifacts.
                // We intentionally do NOT use the alternate screen buffer
                // (\x1b[?1049h) so the final tree state remains visible
                // after the build completes.
                print!("\x1b[?25l\x1b[?7l");
            }
            let _ = io::stdout().flush();
            let mut progress_floor: HashMap<String, f32> = HashMap::new();
            let mut last_frame = String::new();
            let mut prev_line_count: usize = 0;

            loop {
                let snapshot = ui.snapshot();
                let frame = render_tree_frame(&snapshot, &mut progress_floor);
                if frame != last_frame {
                    if use_ansi && prev_line_count > 0 {
                        // Move cursor up to overwrite the previous frame.
                        print!("\x1b[{}A\r\x1b[J", prev_line_count);
                    }
                    print!("{frame}");
                    let _ = io::stdout().flush();
                    prev_line_count = frame.lines().count();
                    last_frame = frame;
                }

                if snapshot.stop {
                    break;
                }
                {
                    let mut state = ui.state.lock().expect("tree state lock poisoned");
                    state.tick = state.tick.wrapping_add(1);
                }
                thread::sleep(Duration::from_millis(90));
            }

            if use_ansi {
                // Restore line wrap + show cursor. The final frame stays on screen.
                print!("\x1b[?7h\x1b[?25h");
                println!();
            }
            let _ = io::stdout().flush();
        })
    }

    fn start_task(&self, id: &str) {
        let mut state = self.state.lock().expect("tree state lock poisoned");
        let tick = state.tick;
        if let Some(node) = state.nodes.get_mut(id) {
            node.status = NodeStatus::Running;
            node.last_line.clear();
            node.fail_log = None;
            node.progress = node.progress.max(0.01);
            node.work_units_seen = 0;
            node.start_tick = tick;
        }
    }

    fn push_line(&self, id: &str, line: String) {
        let mut state = self.state.lock().expect("tree state lock poisoned");
        let tick = state.tick;
        if let Some(node) = state.nodes.get_mut(id) {
            node.last_line = line;
            if node.status == NodeStatus::Running {
                if is_work_unit_line(&node.last_line) {
                    node.work_units_seen = node
                        .work_units_seen
                        .saturating_add(work_units_from_line(&node.last_line));
                }
                let expected = node.work_units_expected.max(1) as f32;
                let by_units = (node.work_units_seen as f32 / expected).clamp(0.0, 0.97);
                let elapsed = tick.saturating_sub(node.start_tick) as f32;
                let by_time = (0.02 + elapsed * 0.0025).min(0.90);
                let candidate = by_units.max(by_time);
                node.progress = node.progress.max(candidate);
            }
        }
    }

    fn finish_task(&self, id: &str, failed: bool, log_path: &Path) {
        let mut state = self.state.lock().expect("tree state lock poisoned");
        if let Some(node) = state.nodes.get_mut(id) {
            if failed {
                node.status = NodeStatus::Failed;
                node.fail_log = Some(relative_to_root_display(log_path));
                node.progress = 1.0;
            } else {
                node.status = NodeStatus::Done;
                node.last_line = "done".to_string();
                node.fail_log = None;
                node.progress = 1.0;
            }
        }
    }

    fn stop(&self) {
        let mut state = self.state.lock().expect("tree state lock poisoned");
        state.stop = true;
    }
}

fn compute_tree_order(
    defs: &[RichTreeNodeDef],
    nodes: &HashMap<String, RichTreeNode>,
) -> Vec<String> {
    let roots: Vec<String> = defs
        .iter()
        .filter(|def| def.parent.is_none())
        .map(|def| def.id.clone())
        .collect();

    let mut order = Vec::new();
    fn walk(id: &str, nodes: &HashMap<String, RichTreeNode>, order: &mut Vec<String>) {
        order.push(id.to_string());
        if let Some(node) = nodes.get(id) {
            for child in &node.children {
                walk(child, nodes, order);
            }
        }
    }
    for root in &roots {
        walk(root, nodes, &mut order);
    }
    order
}

#[cfg(unix)]
fn winsize_from_fd(fd: libc::c_int) -> Option<(usize, usize)> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: ioctl(TIOCGWINSZ) writes into `ws` for a valid tty fd.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col as usize, ws.ws_row as usize))
    } else {
        None
    }
}

fn terminal_dimensions() -> (usize, usize) {
    if let (Some(w), Some(h)) = (
        std::env::var("CLUU_UI_WIDTH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok()),
        std::env::var("CLUU_UI_HEIGHT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok()),
    ) {
        if w > 0 && h > 0 {
            return (w, h);
        }
    }

    #[cfg(unix)]
    {
        let fds = [
            io::stdout().as_raw_fd(),
            io::stderr().as_raw_fd(),
            io::stdin().as_raw_fd(),
        ];
        for fd in fds {
            if let Some((w, h)) = winsize_from_fd(fd) {
                return (w, h);
            }
        }

        if let Ok(tty) = File::open("/dev/tty") {
            if let Some((w, h)) = winsize_from_fd(tty.as_raw_fd()) {
                return (w, h);
            }
        }
    }

    if let Some((terminal_size::Width(w), terminal_size::Height(h))) =
        terminal_size::terminal_size()
    {
        return (w as usize, h as usize);
    }

    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(120);
    let height = std::env::var("LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(40);
    (width, height)
}

fn leaf_progress(node: &RichTreeNode) -> f32 {
    match node.status {
        NodeStatus::Pending => node.progress,
        NodeStatus::Running => node.progress,
        NodeStatus::Done | NodeStatus::Failed => 1.0,
    }
}

fn aggregate_node(
    id: &str,
    nodes: &HashMap<String, RichTreeNode>,
) -> (f32, NodeStatus, Option<String>) {
    let Some(node) = nodes.get(id) else {
        return (0.0, NodeStatus::Pending, None);
    };
    if node.children.is_empty() || node.is_leaf {
        return (leaf_progress(node), node.status, node.fail_log.clone());
    }

    let mut progress_sum = 0.0f32;
    let mut count = 0usize;
    let mut any_running = false;
    let mut any_done = false;
    let mut any_failed = false;
    let mut all_done = true;
    let mut first_fail_log: Option<String> = None;

    for child in &node.children {
        let (progress, status, fail_log) = aggregate_node(child, nodes);
        progress_sum += progress;
        count += 1;
        match status {
            NodeStatus::Failed => {
                any_failed = true;
                if first_fail_log.is_none() {
                    first_fail_log = fail_log;
                }
            }
            NodeStatus::Running => any_running = true,
            NodeStatus::Done => any_done = true,
            NodeStatus::Pending => {}
        }
        if status != NodeStatus::Done {
            all_done = false;
        }
    }

    let progress = if count == 0 {
        0.0
    } else {
        progress_sum / (count as f32)
    };
    let status = if any_failed {
        NodeStatus::Failed
    } else if count > 0 && all_done {
        NodeStatus::Done
    } else if any_running || any_done {
        NodeStatus::Running
    } else {
        NodeStatus::Pending
    };

    (progress, status, first_fail_log)
}

fn tree_prefix(id: &str, nodes: &HashMap<String, RichTreeNode>) -> (String, bool) {
    let mut parent_chain: Vec<(&str, &str)> = Vec::new();
    let mut current = id;
    while let Some(node) = nodes.get(current) {
        if let Some(parent) = node.parent.as_deref() {
            parent_chain.push((current, parent));
            current = parent;
        } else {
            break;
        }
    }

    let mut prefix = String::new();
    for (idx, (child, parent)) in parent_chain.iter().rev().enumerate() {
        let parent_node = match nodes.get(*parent) {
            Some(node) => node,
            None => continue,
        };
        let is_last = parent_node
            .children
            .last()
            .map(|last| last.as_str() == *child)
            .unwrap_or(true);
        let is_direct_parent = idx + 1 == parent_chain.len();
        if is_direct_parent {
            prefix.push_str(if is_last { "└─ " } else { "├─ " });
        } else {
            prefix.push_str(if is_last { "   " } else { "│  " });
        }
    }

    let is_root = parent_chain.is_empty();
    (prefix, is_root)
}

fn should_use_color() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn should_use_ansi_controls() -> bool {
    if !io::stdout().is_terminal() {
        return false;
    }
    match std::env::var("TERM") {
        Ok(term) => !term.trim().is_empty() && term != "dumb",
        Err(_) => true,
    }
}

fn paint(text: &str, ansi_code: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[{}m{}\x1b[0m", ansi_code, text)
    } else {
        text.to_string()
    }
}

fn colorize_tree_prefix(prefix: &str, use_color: bool) -> String {
    if !use_color {
        return prefix.to_string();
    }
    let mut out = String::with_capacity(prefix.len() * 2);
    for ch in prefix.chars() {
        match ch {
            '├' | '└' | '│' | '─' => out.push_str(&paint(&ch.to_string(), "38;5;45", true)),
            _ => out.push(ch),
        }
    }
    out
}

fn progress_bar(progress: f32, width: usize, status: NodeStatus, use_color: bool) -> String {
    let width = width.max(4);
    let filled = ((progress.clamp(0.0, 1.0)) * (width as f32)).round() as usize;
    let (fill_color, empty_color) = match status {
        NodeStatus::Pending => ("90", "90"),
        NodeStatus::Running => ("38;5;45", "90"),
        NodeStatus::Done => ("32", "90"),
        NodeStatus::Failed => ("31", "90"),
    };
    let mut out = String::with_capacity(width * 8);
    for i in 0..width {
        if i < filled {
            out.push_str(&paint("#", fill_color, use_color));
        } else {
            out.push_str(&paint("-", empty_color, use_color));
        }
    }
    out
}

fn colorize_status(status: NodeStatus, use_color: bool) -> String {
    match status {
        NodeStatus::Pending => paint("PENDING", "90", use_color),
        NodeStatus::Running => paint("RUNNING", "33", use_color),
        NodeStatus::Done => paint("DONE", "32", use_color),
        NodeStatus::Failed => paint("FAILED", "31", use_color),
    }
}

fn status_counts(snapshot: &RichTreeSnapshot) -> (usize, usize, usize, usize, usize) {
    let mut leaves = 0usize;
    let mut pending = 0usize;
    let mut running = 0usize;
    let mut done = 0usize;
    let mut failed = 0usize;

    for node in snapshot.nodes.values() {
        if !node.is_leaf {
            continue;
        }
        leaves += 1;
        match node.status {
            NodeStatus::Pending => pending += 1,
            NodeStatus::Running => running += 1,
            NodeStatus::Done => done += 1,
            NodeStatus::Failed => failed += 1,
        }
    }

    (leaves, pending, running, done, failed)
}

fn render_tree_frame(
    snapshot: &RichTreeSnapshot,
    progress_floor: &mut HashMap<String, f32>,
) -> String {
    let (term_width, term_height) = terminal_dimensions();
    let width = term_width.max(24);
    let visible_lines = term_height.max(1);
    let use_color = should_use_color();
    let mut out = String::new();
    let (overall_progress, _, _) = aggregate_node("build", &snapshot.nodes);
    let overall_percent = ((overall_progress.clamp(0.0, 1.0)) * 100.0).round() as u32;
    let overall_bar = progress_bar(overall_progress, 28, NodeStatus::Running, use_color);
    let (total, pending, running, done, failed) = status_counts(snapshot);
    let run_id = Path::new(&snapshot.logs_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let header_line_1 = format!(
        "{}  [{}]  {}",
        paint(&snapshot.title, "1;97", use_color),
        overall_bar,
        paint(&format!("{:>3}%", overall_percent), "1;33", use_color)
    );
    let header_line_2 = format!(
        "{} {}  {} {}  {} {}  {} {}  {} {}",
        paint("run:", "90", use_color),
        paint(run_id, "96", use_color),
        paint("done:", "90", use_color),
        paint(&done.to_string(), "32", use_color),
        paint("running:", "90", use_color),
        paint(&running.to_string(), "33", use_color),
        paint("pending:", "90", use_color),
        paint(&pending.to_string(), "90", use_color),
        paint("failed:", "90", use_color),
        paint(
            &failed.to_string(),
            if failed > 0 { "31;1" } else { "32" },
            use_color
        )
    );
    let header_line_3 = format!(
        "{} {}",
        paint("tasks:", "90", use_color),
        paint(&total.to_string(), "94", use_color)
    );
    let header_line_4 = paint(
        "hint: use 'cargo xtask logs --task <name> --follow' for live per-task logs",
        "2",
        use_color,
    );
    let separator = paint(&"─".repeat(width), "38;5;45", use_color);
    let mut header_lines = vec![
        header_line_1,
        header_line_2,
        header_line_3,
        header_line_4,
        separator,
    ];

    let mut lines: Vec<String> = Vec::new();
    for id in &snapshot.order {
        let Some(node) = snapshot.nodes.get(id.as_str()) else {
            continue;
        };
        let (raw_progress, status, fail_log) = aggregate_node(&node.id, &snapshot.nodes);
        let previous = progress_floor.get(id).copied().unwrap_or(0.0);
        let progress = match status {
            NodeStatus::Done | NodeStatus::Failed => 1.0,
            NodeStatus::Pending | NodeStatus::Running => raw_progress.max(previous),
        }
        .clamp(0.0, 1.0);
        progress_floor.insert(id.clone(), progress);
        let (prefix, is_root) = tree_prefix(&node.id, &snapshot.nodes);
        let prefix = colorize_tree_prefix(&prefix, use_color);
        let label = if is_root {
            paint(&node.label, "1;97", use_color)
        } else {
            format!("{}{}", prefix, paint(&node.label, "36", use_color))
        };

        let percent = ((progress.clamp(0.0, 1.0)) * 100.0).round() as u32;
        let percent_s = match status {
            NodeStatus::Pending => paint(&format!("{:>3}%", percent), "90", use_color),
            NodeStatus::Running => paint(&format!("{:>3}%", percent), "33", use_color),
            NodeStatus::Done => paint(&format!("{:>3}%", percent), "32", use_color),
            NodeStatus::Failed => paint(&format!("{:>3}%", percent), "31", use_color),
        };
        let status_s = colorize_status(status, use_color);

        let make_base_line = |bar_width: usize| {
            let bar = progress_bar(progress, bar_width, status, use_color);
            format!("{} [{}] {} {}", label, bar, percent_s, status_s)
        };

        // Fit the base line first so tree/progress/status stay intact on narrow terminals.
        let mut bar_width = 18usize;
        let mut line = loop {
            let candidate = make_base_line(bar_width);
            if visible_width_ansi(&candidate) <= width || bar_width <= 4 {
                break candidate;
            }
            bar_width -= 1;
        };

        if status == NodeStatus::Running && !node.last_line.is_empty() {
            // Reserve visible room for a live log snippet by shrinking the bar if needed.
            let desired_live_width = 24usize;
            loop {
                let remaining = width.saturating_sub(visible_width_ansi(&line) + 2);
                if remaining >= desired_live_width || bar_width <= 4 {
                    break;
                }
                bar_width -= 1;
                line = make_base_line(bar_width);
            }
            let remaining = width.saturating_sub(visible_width_ansi(&line) + 2);
            if remaining > 0 {
                line.push_str("  ");
                line.push_str(&paint(
                    &truncate_live_line(&node.last_line, remaining.max(1)),
                    "2",
                    use_color,
                ));
            }
        }
        if status == NodeStatus::Failed {
            let note = fail_log.unwrap_or_else(|| "log unavailable".to_string());
            let remaining = width.saturating_sub(visible_width_ansi(&line) + 2);
            if remaining > 6 {
                line.push_str("  ");
                line.push_str(&paint(
                    &truncate_live_line(&format!("log: {}", note), remaining),
                    "31;1",
                    use_color,
                ));
            }
        }
        lines.push(line);
    }

    let header_count = header_lines.len();
    let tree_visible = visible_lines.saturating_sub(header_count);
    let mut rendered_lines: Vec<String> = Vec::new();
    rendered_lines.extend(
        header_lines
            .drain(..)
            .take(visible_lines)
            .map(|line| fit_render_line(&line, width, use_color)),
    );
    rendered_lines.extend(
        lines
            .into_iter()
            .take(tree_visible)
            .map(|line| fit_render_line(&line, width, use_color)),
    );
    out.push_str(&rendered_lines.join("\n"));
    out
}

type TaskSink = Arc<dyn Fn(String) + Send + Sync + 'static>;

fn stream_child_pipe<R: Read + Send + 'static>(
    reader: R,
    log_file: Arc<Mutex<File>>,
    sink: TaskSink,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut bytes = Vec::<u8>::new();
            let n = reader.read_until(b'\n', &mut bytes)?;
            if n == 0 {
                break;
            }

            {
                let mut file = log_file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Log file lock poisoned"))?;
                file.write_all(&bytes)?;
                file.flush()?;
            }

            let text = String::from_utf8_lossy(&bytes);
            let line = sanitize_live_line(&text);
            if !line.is_empty() {
                sink(line);
            }
        }
        Ok(())
    })
}

fn run_internal_xtask_task(args: &[String], log_path: &Path, sink: TaskSink) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to locate xtask executable")?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let log_file = File::create(log_path)
        .with_context(|| format!("Failed to create task log at {}", log_path.display()))?;
    let log_file = Arc::new(Mutex::new(log_file));
    let mut child = Command::new(exe)
        .current_dir(project_root())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run internal xtask command: {:?}", args))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture child stdout"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture child stderr"))?;

    let stdout_thread = stream_child_pipe(child_stdout, log_file.clone(), sink.clone());
    let stderr_thread = stream_child_pipe(child_stderr, log_file, sink);
    let status = child.wait().context("Failed to wait for internal task")?;

    stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdout stream thread panicked"))??;
    stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stderr stream thread panicked"))??;

    if !status.success() {
        bail!("Internal task failed: {:?}", args);
    }
    Ok(())
}

fn read_log_tail(log_path: &Path, lines: usize) -> String {
    let Ok(content) = fs::read_to_string(log_path) else {
        return String::new();
    };
    let mut collected: Vec<&str> = content.lines().rev().take(lines).collect();
    collected.reverse();
    collected.join("\n")
}

fn run_rich_task(task: &RichTask, logs_dir: &Path, ui: Option<RichTreeUi>) -> Result<()> {
    let task_name = task.name.clone();
    let log_path = logs_dir.join(format!("{}.log", task_name));
    if let Some(ui_ref) = ui.as_ref() {
        ui_ref.start_task(&task_name);
    }

    let sink: TaskSink = if let Some(ui_ref) = ui.clone() {
        let name_for_sink = task_name.clone();
        Arc::new(move |line: String| {
            ui_ref.push_line(&name_for_sink, line);
        })
    } else {
        let name_for_sink = task_name.clone();
        Arc::new(move |line: String| {
            println!("[{}] {}", name_for_sink, line);
        })
    };

    let result = run_internal_xtask_task(&task.args, &log_path, sink);
    match result {
        Ok(()) => {
            if let Some(ui_ref) = ui.as_ref() {
                ui_ref.finish_task(&task_name, false, &log_path);
            }
            Ok(())
        }
        Err(err) => {
            if let Some(ui_ref) = ui.as_ref() {
                ui_ref.finish_task(&task_name, true, &log_path);
            }
            Err(err.context(format!(
                "Task '{}' failed. Log: {}",
                task_name,
                log_path.display()
            )))
        }
    }
}

fn run_rich_dag(
    tasks: Vec<RichTask>,
    tree_defs: Vec<RichTreeNodeDef>,
    logs_dir: &Path,
    interactive_tree: bool,
) -> Result<()> {
    let mut pending: HashMap<String, RichTask> = tasks
        .into_iter()
        .map(|task| (task.name.clone(), task))
        .collect();
    let mut completed: HashSet<String> = HashSet::new();
    let mut running: HashSet<String> = HashSet::new();
    let (tx, rx) = mpsc::channel::<(String, Result<()>)>();
    let mut first_error: Option<anyhow::Error> = None;

    let tree_ui = if interactive_tree {
        Some(RichTreeUi::new(
            "CLUU rich build".to_string(),
            logs_dir.to_path_buf(),
            &tree_defs,
        ))
    } else {
        None
    };
    let renderer = tree_ui.as_ref().map(|ui| ui.start_renderer());

    loop {
        if first_error.is_none() {
            let ready: Vec<String> = pending
                .iter()
                .filter(|(_, task)| task.deps.iter().all(|dep| completed.contains(dep)))
                .map(|(name, _)| name.clone())
                .collect();

            for name in ready {
                let task = pending.remove(&name).expect("task must exist");
                running.insert(name);

                let logs_dir = logs_dir.to_path_buf();
                let tx = tx.clone();
                let tree_ui = tree_ui.clone();

                thread::spawn(move || {
                    let name = task.name.clone();
                    let result = run_rich_task(&task, &logs_dir, tree_ui);
                    let _ = tx.send((name, result));
                });
            }
        }

        if running.is_empty() {
            if let Some(ui) = tree_ui.as_ref() {
                ui.stop();
            }
            if let Some(handle) = renderer {
                let _ = handle.join();
            }

            if let Some(err) = first_error {
                return Err(err);
            }
            if pending.is_empty() {
                return Ok(());
            }

            let unresolved = pending.keys().cloned().collect::<Vec<_>>().join(", ");
            bail!(
                "Task dependency deadlock: unresolved tasks [{}]",
                unresolved
            );
        }

        let (finished_name, result) = rx.recv().context("Failed to receive task completion")?;
        running.remove(&finished_name);
        completed.insert(finished_name);

        if let Err(err) = result {
            if first_error.is_none() {
                first_error = Some(err);
            }
        }

        if first_error.is_some() && running.is_empty() {
            continue;
        }
    }
}

fn build_pipeline_rich(profile: &str) -> Result<()> {
    let logs_dir = rich_log_dir()?;
    println!("▸ Build UI: rich");
    println!("  Logs: {}", logs_dir.display());
    println!("  Switch to linear mode for verbose output: --ui linear");

    let container_names = discover_containers();

    // -- Tree node definitions --------------------------------------------------
    let mut tree_defs = vec![
        RichTreeNodeDef {
            id: "build".into(),
            label: "build".into(),
            parent: None,
            is_leaf: false,
        },
        // Dependencies
        RichTreeNodeDef {
            id: "dependencies".into(),
            label: "dependencies".into(),
            parent: Some("build".into()),
            is_leaf: false,
        },
        RichTreeNodeDef {
            id: "dep-klibcluu".into(),
            label: "klibcluu".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        RichTreeNodeDef {
            id: "dep-libcluu".into(),
            label: "libcluu".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        RichTreeNodeDef {
            id: "dep-newlib".into(),
            label: "newlib".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        RichTreeNodeDef {
            id: "dep-syscalls".into(),
            label: "syscalls".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        RichTreeNodeDef {
            id: "dep-crt0".into(),
            label: "crt0".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        // Kernel
        RichTreeNodeDef {
            id: "kernel".into(),
            label: "kernel".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
        // Userspace
        RichTreeNodeDef {
            id: "userspace".into(),
            label: "userspace".into(),
            parent: Some("build".into()),
            is_leaf: false,
        },
        RichTreeNodeDef {
            id: "init".into(),
            label: "init".into(),
            parent: Some("userspace".into()),
            is_leaf: false,
        },
        RichTreeNodeDef {
            id: "containers".into(),
            label: "containers".into(),
            parent: Some("userspace".into()),
            is_leaf: false,
        },
        // Packaging
        RichTreeNodeDef {
            id: "initrd".into(),
            label: "initrd".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
        RichTreeNodeDef {
            id: "userdisk".into(),
            label: "userdisk".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
        RichTreeNodeDef {
            id: "disk-image".into(),
            label: "disk-image".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
    ];

    // Init primordial subtasks (one per crate)
    for crate_name in INIT_CRATES {
        tree_defs.push(RichTreeNodeDef {
            id: format!("init-{}", crate_name),
            label: crate_name.to_string(),
            parent: Some("init".into()),
            is_leaf: true,
        });
    }

    // Dynamic container nodes (auto-discovered from containers/*/)
    for name in &container_names {
        tree_defs.push(RichTreeNodeDef {
            id: format!("container-{}", name),
            label: name.clone(),
            parent: Some("containers".into()),
            is_leaf: true,
        });
    }

    // -- Task DAG ---------------------------------------------------------------
    // Dependencies: klibcluu, libcluu, newlib, crt0 start immediately.
    //               syscalls waits for libcluu (Cargo dependency).
    // Kernel:       waits for all deps.
    // Init:         each crate waits for all deps; serializes via cargo lock.
    // Containers:   each waits for all init crates (need warm cache).
    // Packaging:    initrd waits for kernel + all init + all containers.
    //               userdisk waits for all init + all containers.
    //               disk-image waits for initrd + userdisk.
    let all_dep_names: Vec<String> = vec![
        "dep-klibcluu".into(),
        "dep-libcluu".into(),
        "dep-newlib".into(),
        "dep-syscalls".into(),
        "dep-crt0".into(),
    ];

    let init_task_ids: Vec<String> = INIT_CRATES.iter().map(|n| format!("init-{}", n)).collect();

    let mut tasks = vec![
        // Dependencies
        RichTask {
            name: "dep-klibcluu".into(),
            args: vec!["build-klibcluu".into()],
            deps: vec![],
        },
        RichTask {
            name: "dep-libcluu".into(),
            args: vec!["build-libcluu".into(), "--profile".into(), profile.into()],
            deps: vec![],
        },
        RichTask {
            name: "dep-newlib".into(),
            args: vec!["build-newlib".into()],
            deps: vec![],
        },
        RichTask {
            name: "dep-syscalls".into(),
            args: vec!["build-syscalls".into(), "--profile".into(), profile.into()],
            deps: vec!["dep-libcluu".into()],
        },
        RichTask {
            name: "dep-crt0".into(),
            args: vec!["build-crt0".into()],
            deps: vec![],
        },
        // Kernel
        RichTask {
            name: "kernel".into(),
            args: vec!["kernel".into(), "--profile".into(), profile.into()],
            deps: all_dep_names.clone(),
        },
    ];

    // Init primordial tasks — each depends on all deps
    for crate_name in INIT_CRATES {
        tasks.push(RichTask {
            name: format!("init-{}", crate_name),
            args: vec![
                "build-init-crate".into(),
                crate_name.to_string(),
                "--profile".into(),
                profile.into(),
            ],
            deps: all_dep_names.clone(),
        });
    }

    // Dynamic container tasks (each depends on all init crates for warm cache)
    let container_task_ids: Vec<String> = container_names
        .iter()
        .map(|n| format!("container-{}", n))
        .collect();
    for name in &container_names {
        tasks.push(RichTask {
            name: format!("container-{}", name),
            args: vec!["build-single-container".into(), name.clone()],
            deps: init_task_ids.clone(),
        });
    }

    // Packaging
    let mut initrd_deps = vec!["kernel".into()];
    initrd_deps.extend(init_task_ids.iter().cloned());
    initrd_deps.extend(container_task_ids.iter().cloned());

    let mut userdisk_deps: Vec<String> = init_task_ids.clone();
    userdisk_deps.extend(container_task_ids.iter().cloned());

    tasks.push(RichTask {
        name: "initrd".into(),
        args: vec!["create-initrd".into(), "--profile".into(), profile.into()],
        deps: initrd_deps,
    });
    tasks.push(RichTask {
        name: "userdisk".into(),
        args: vec![
            "create-user-block-image".into(),
            "--profile".into(),
            profile.into(),
        ],
        deps: userdisk_deps,
    });
    tasks.push(RichTask {
        name: "disk-image".into(),
        args: vec![
            "create-disk-image".into(),
            "--profile".into(),
            profile.into(),
        ],
        deps: vec!["initrd".into(), "userdisk".into()],
    });

    let interactive_tree = io::stdout().is_terminal();
    run_rich_dag(tasks, tree_defs, &logs_dir, interactive_tree)?;

    println!("✓ Rich build complete");
    println!("  Per-task logs are in {}", logs_dir.display());
    Ok(())
}

fn run_harness_matrix(no_build: bool) -> Result<()> {
    println!("▸ Running harness matrix...");
    let script = project_root().join("scripts/harness_matrix.sh");
    let mut cmd = Command::new("bash");
    cmd.current_dir(project_root()).arg(script);
    if no_build {
        cmd.arg("--no-build");
    }

    let status = cmd
        .status()
        .context("Failed to run harness matrix script")?;
    if !status.success() {
        bail!("Harness matrix failed");
    }
    println!("  ✓ Harness matrix passed");
    Ok(())
}

fn run_harness_slo(no_build: bool, repeats: u32) -> Result<()> {
    println!("▸ Running harness SLO sweep (repeats={repeats})...");
    let script = project_root().join("scripts/harness_slo_sweep.sh");
    let mut cmd = Command::new("bash");
    cmd.current_dir(project_root())
        .arg(script)
        .arg("--repeats")
        .arg(repeats.to_string());
    if no_build {
        cmd.arg("--no-build");
    }

    let status = cmd.status().context("Failed to run harness SLO script")?;
    if !status.success() {
        bail!("Harness SLO sweep failed");
    }
    println!("  ✓ Harness SLO sweep passed");
    Ok(())
}

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn parse_config_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let entry = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (key, raw_value) = entry.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let mut value = raw_value.trim().to_string();
    if value.len() >= 2 {
        let quoted = (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''));
        if quoted {
            value = value[1..value.len() - 1].to_string();
        }
    }
    Some((key.to_string(), value))
}

fn parse_csv_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn resolve_config_path(root: &Path, maybe_relative: &str) -> PathBuf {
    let path = PathBuf::from(maybe_relative);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn relative_to_root_display(path: &Path) -> String {
    let root = project_root();
    match path.strip_prefix(&root) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

fn load_external_sources_config() -> Result<ExternalSourcesConfig> {
    let root = project_root();
    let config_path = root.join(EXTERNAL_SOURCES_CONFIG_REL);
    let mut map = HashMap::<String, String>::new();

    if config_path.exists() {
        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {:?}", config_path))?;
        for line in contents.lines() {
            if let Some((key, value)) = parse_config_line(line) {
                map.insert(key, value);
            }
        }
    }

    let newlib_version = map
        .get("CLUU_NEWLIB_VERSION")
        .cloned()
        .unwrap_or_else(|| DEFAULT_NEWLIB_VERSION.to_string());
    let newlib_url = map.get("CLUU_NEWLIB_URL").cloned().unwrap_or_else(|| {
        format!(
            "ftp://sourceware.org/pub/newlib/newlib-{}.tar.gz",
            newlib_version
        )
    });
    let newlib_dir_rel = map
        .get("CLUU_NEWLIB_DIR")
        .cloned()
        .unwrap_or_else(|| format!("external/newlib-{}", newlib_version));
    let newlib_patch_files = map
        .get("CLUU_NEWLIB_PATCH_FILES")
        .map(|raw| parse_csv_list(raw))
        .unwrap_or_default();

    let micropython_version = map
        .get("CLUU_MICROPYTHON_VERSION")
        .cloned()
        .unwrap_or_else(|| DEFAULT_MICROPYTHON_VERSION.to_string());
    let micropython_repo = map
        .get("CLUU_MICROPYTHON_REPO")
        .cloned()
        .unwrap_or_else(|| "https://github.com/micropython/micropython.git".to_string());
    let micropython_ref = map
        .get("CLUU_MICROPYTHON_REF")
        .cloned()
        .or_else(|| Some(DEFAULT_MICROPYTHON_REF.to_string()));
    let micropython_dir_rel = map
        .get("CLUU_MICROPYTHON_DIR")
        .cloned()
        .unwrap_or_else(|| "external/micropython".to_string());
    let micropython_patch_files = map
        .get("CLUU_MICROPYTHON_PATCH_FILES")
        .map(|raw| parse_csv_list(raw))
        .unwrap_or_default();

    Ok(ExternalSourcesConfig {
        config_path,
        newlib_version,
        newlib_url,
        newlib_dir: resolve_config_path(&root, &newlib_dir_rel),
        newlib_patch_files,
        micropython_version,
        micropython_repo,
        micropython_ref,
        micropython_dir: resolve_config_path(&root, &micropython_dir_rel),
        micropython_patch_files,
    })
}

fn git_capture_stdout(repo_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_lines_stdout(repo_dir: &Path, args: &[&str]) -> Vec<String> {
    let Some(stdout) = git_capture_stdout(repo_dir, args) else {
        return Vec::new();
    };
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn git_ls_files_under(path: &Path) -> Vec<String> {
    let root = project_root();
    let rel = path.strip_prefix(&root).unwrap_or(path);
    let rel_str = rel.to_string_lossy().to_string();
    if rel_str.is_empty() {
        return Vec::new();
    }
    git_lines_stdout(&root, &["ls-files", "--", rel_str.as_str()])
}

fn nested_repo_has_tracked_modifications(repo_dir: &Path) -> bool {
    !git_lines_stdout(repo_dir, &["status", "--porcelain", "--untracked-files=no"]).is_empty()
}

fn run_script(script_relative: &str) -> Result<()> {
    let script_path = project_root().join(script_relative);
    if !script_path.exists() {
        bail!("Required script not found: {}", script_path.display());
    }

    let status = Command::new("bash")
        .current_dir(project_root())
        .arg(&script_path)
        .status()
        .with_context(|| format!("Failed to run {}", script_path.display()))?;
    if !status.success() {
        bail!("Script failed: {}", script_path.display());
    }
    Ok(())
}

fn ensure_newlib_source() -> Result<()> {
    run_script("scripts/download-newlib.sh")
}

fn ensure_micropython_source() -> Result<()> {
    run_script("scripts/download-micropython.sh")
}

fn build_userspace(profile: &str) -> Result<()> {
    println!("▸ Building userspace programs...");

    let userspace_crates = [
        "userspace/libcluu",    // Library must be first
        "userspace/virtio-blk", // Driver library
        "userspace/ext2",       // Filesystem library
        "userspace/init",       // System programs
        "userspace/procmgr",
        "userspace/registry",
        "userspace/vfs",
        "userspace/ramfs",
        "userspace/console",
        "userspace/kbd",
        "userspace/tty",
        "userspace/vtmgr",
        "userspace/shell",
        "userspace/timeserver",
        "userspace/cat",
        "userspace/tpmd",
    ];

    let target_json = project_root().join("triplets/x86_64-cluu-user.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    for crate_path in &userspace_crates {
        let crate_name = Path::new(crate_path).file_name().unwrap().to_str().unwrap();

        println!("  Building {}...", crate_name);

        let mut cmd = Command::new("cargo");
        cmd.current_dir(project_root()).args([
            "build",
            "--manifest-path",
            &format!("{}/Cargo.toml", crate_path),
            "--target",
            target_json.to_str().unwrap(),
            "-Z",
            "build-std=core,alloc",
            "-Z",
            "build-std-features=compiler-builtins-mem",
        ]);
        cmd.env("TMPDIR", tmp_dir.as_os_str());

        if profile == "release" {
            cmd.arg("--release");
        }

        let status = cmd.status().context("Failed to run cargo")?;
        if !status.success() {
            bail!("Failed to build {}", crate_name);
        }
    }

    println!("  ✓ Userspace built");
    Ok(())
}

fn build_kernel(profile: &str) -> Result<()> {
    println!("▸ Building kernel...");

    let target_json = project_root().join("triplets/x86_64-cluu-kernel.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    // First, assemble NASM files if they exist
    let _ = assemble_nasm(profile);

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root()).args([
        "build",
        "--manifest-path",
        "kernel/Cargo.toml",
        "--target",
        target_json.to_str().unwrap(),
        "-Z",
        "build-std=core,alloc",
        "-Z",
        "build-std-features=compiler-builtins-mem",
    ]);

    cmd.env("TMPDIR", tmp_dir.as_os_str());

    if profile == "release" {
        cmd.arg("--release");
    }

    let status = cmd.status().context("Failed to run cargo")?;
    if !status.success() {
        bail!("Failed to build kernel");
    }

    println!("  ✓ Kernel built");
    Ok(())
}

fn assemble_nasm(profile: &str) -> Result<()> {
    println!("  Assembling NASM files...");

    let asm_dir = project_root().join("kernel/src/architecture/x86_64");
    let out_dir = project_root().join("target/asm");

    fs::create_dir_all(&out_dir)?;

    let asm_files = [
        "boot.asm",
        "context.asm",
        "interrupts.asm",
        "syscall_entry.asm",
    ];

    for asm_file in &asm_files {
        let src = asm_dir.join(asm_file);
        if !src.exists() {
            continue; // Skip if file doesn't exist yet
        }

        let obj_name = asm_file.replace(".asm", ".o");
        let obj = out_dir.join(&obj_name);

        let mut cmd = Command::new("nasm");
        cmd.args(["-f", "elf64", "-g", "-F", "dwarf"]);

        // Define DEBUG symbol for non-release builds so %ifdef DEBUG
        // sections (e.g. telemetry stores in syscall_entry.asm) are active.
        if profile != "release" {
            cmd.arg("-dDEBUG");
        }

        cmd.args(["-o", obj.to_str().unwrap(), src.to_str().unwrap()]);

        let status = cmd.status().context("Failed to run NASM")?;

        if !status.success() {
            bail!("NASM failed for {}", asm_file);
        }
    }

    Ok(())
}

fn create_initrd(profile: &str) -> Result<()> {
    println!("▸ Creating initrd...");

    // Cargo uses "debug" for dev profile and "release" for release profile
    let cargo_profile = if profile == "dev" { "debug" } else { profile };

    let kernel_target_dir = project_root()
        .join("target/x86_64-cluu-kernel")
        .join(cargo_profile);

    let userspace_target_dir = project_root()
        .join("target/x86_64-cluu-user")
        .join(cargo_profile);

    let initrd_dir = project_root().join("target/initrd");

    // Clean and recreate to remove stale files from previous builds.
    if initrd_dir.exists() {
        fs::remove_dir_all(&initrd_dir)?;
    }
    fs::create_dir_all(initrd_dir.join("sys"))?;
    fs::create_dir_all(initrd_dir.join("etc"))?;

    // Copy kernel as sys/core (BOOTBOOT convention)
    let deps_dir = kernel_target_dir.join("deps");

    if !deps_dir.exists() {
        bail!("Kernel deps directory not found at {:?}", deps_dir);
    }

    let kernel_src = deps_dir
        .read_dir()
        .context("Failed to read kernel deps directory")?
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name();
            let name_str = name.to_str().unwrap_or("");
            name_str.starts_with("kernel-") && name_str.ends_with(".elf")
        });

    if let Some(kernel_entry) = kernel_src {
        let dst = initrd_dir.join("sys/core");
        fs::copy(kernel_entry.path(), &dst).context("Failed to copy kernel as sys/core")?;
        println!("  Copied kernel as sys/core");
    } else {
        bail!("Kernel binary not found in {:?}", deps_dir);
    }

    // Copy init primordials to initrd/sys/ — only the services
    // bootstrapped by init before procmgr takes over.  Everything else
    // (console, kbd, tty, vtmgr, shell) is loaded from ext2 containers.
    let sys_programs = [
        "init",
        "registry",
        "timeserver",
        "procmgr",
        "vfs",
        "virtio-blk",
        "tpmd",
    ];
    let mut copied_sys_paths = Vec::new();
    for prog in &sys_programs {
        let src = userspace_target_dir.join(format!("{}.elf", prog));
        let dst = initrd_dir.join("sys").join(prog);
        if src.exists() {
            fs::copy(&src, &dst).with_context(|| format!("Failed to copy {}", prog))?;
            println!("  Copied sys/{}", prog);
            copied_sys_paths.push(format!("sys/{}", prog));
        } else {
            bail!("Required system binary '{}' not found at {:?}", prog, src);
        }
    }

    // Note: shell and other userspace programs are loaded from ext2 containers,
    // not from the initrd.

    // Create etc/motd
    fs::write(initrd_dir.join("etc/motd"), "Welcome to CLUU!\n")?;

    // Create mandatory boot manifest for init policy checks.
    let manifest = build_boot_manifest(&initrd_dir, &copied_sys_paths)?;
    fs::write(initrd_dir.join("sys/boot.manifest"), manifest)?;
    println!("  Wrote sys/boot.manifest");

    println!("  ✓ initrd directory created");
    Ok(())
}

fn build_boot_manifest(initrd_dir: &Path, service_paths: &[String]) -> Result<String> {
    let mut canonical = String::from("manifest_version=1\n");
    for path in service_paths {
        let data = fs::read(initrd_dir.join(path))
            .with_context(|| format!("Failed to read service image '{}'", path))?;
        let digest = hash_sha256(&data);
        let digest_hex = to_lower_hex(&digest);
        let rights_mask = manifest_rights_mask(path);
        canonical.push_str(&format!(
            "service path={} sha256={} rights=0x{:08x}\n",
            path, digest_hex, rights_mask
        ));
    }
    let signature = to_lower_hex(&hmac_sha256_fixed(
        &BOOT_MANIFEST_HMAC_KEY,
        canonical.as_bytes(),
    ));

    let mut out = String::from("# CLUU boot manifest\n");
    out.push_str(&canonical);
    out.push_str(&format!("signature={}\n", signature));
    Ok(out)
}

fn manifest_rights_mask(path: &str) -> u32 {
    match path {
        // Mirrors userspace/init/src/services.rs
        "sys/procmgr" => {
            RIGHT_READ
                | RIGHT_WRITE
                | RIGHT_CREATE
                | RIGHT_THREAD_CONTROL
                | RIGHT_THREAD_SUSPEND
                | RIGHT_DESTROY
                | RIGHT_SPACE_MAP
                | RIGHT_SPACE_UNMAP
                | RIGHT_SPACE_GRANT
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_IPC_CALL
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
                | RIGHT_GRANT
        }
        "sys/virtio-blk" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
        }
        "sys/tpmd" => {
            RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
        }
        _ => ALL_RIGHTS_MASK,
    }
}

fn hash_sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn hmac_sha256_fixed(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    const IPAD: u8 = 0x36;
    const OPAD: u8 = 0x5c;

    let mut key_block = [0u8; BLOCK_SIZE];
    key_block[..32].copy_from_slice(key);

    let mut inner = Vec::with_capacity(BLOCK_SIZE + data.len());
    for b in key_block.iter().take(BLOCK_SIZE) {
        inner.push(*b ^ IPAD);
    }
    inner.extend_from_slice(data);
    let inner_hash = hash_sha256(&inner);

    let mut outer = [0u8; BLOCK_SIZE + 32];
    for (i, b) in key_block.iter().enumerate().take(BLOCK_SIZE) {
        outer[i] = *b ^ OPAD;
    }
    outer[BLOCK_SIZE..BLOCK_SIZE + 32].copy_from_slice(&inner_hash);
    hash_sha256(&outer)
}

fn to_lower_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn create_disk_image(_profile: &str) -> Result<()> {
    println!("▸ Creating disk image...");

    // Create mkbootimg.json configuration
    let mkbootimg_json = r#"{
    "disksize": 128,
    "config": "target/bootboot_config",
    "initrd": { "type": "tar", "gzip": true, "directory": "target/initrd" },
    "iso9660": true,
    "partitions": [
        { "type": "boot", "size": 16 }
    ]
}"#;

    let config_path = project_root().join("target/mkbootimg.json");
    fs::write(&config_path, mkbootimg_json)?;

    // Create bootboot config file. Optional extra BOOTBOOT environment lines
    // can be injected via CLUU_BOOTBOOT_ENV (newline or ';' separated).
    let mut bootboot_config =
        String::from("// BOOTBOOT configuration\nscreen=1280x720\nkernel=sys/core\n");
    if let Ok(extra_env) = std::env::var("CLUU_BOOTBOOT_ENV") {
        for line in extra_env
            .split(['\n', ';'])
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            bootboot_config.push_str(line);
            bootboot_config.push('\n');
        }
    }
    fs::write(
        project_root().join("target/bootboot_config"),
        bootboot_config,
    )?;

    let output_img = project_root().join("target/cluu.img");

    // Check if mkbootimg exists
    let mkbootimg_path = project_root().join("tools/mkbootimg/mkbootimg");
    if !mkbootimg_path.exists() {
        println!("  Building mkbootimg...");
        let status = Command::new("make")
            .current_dir(project_root().join("tools/mkbootimg"))
            .arg("all")
            .status()
            .context("Failed to build mkbootimg")?;

        if !status.success() {
            bail!("Failed to build mkbootimg");
        }
    }

    let status = Command::new(mkbootimg_path)
        .current_dir(project_root())
        .args([config_path.to_str().unwrap(), output_img.to_str().unwrap()])
        .status()
        .context("Failed to run mkbootimg")?;

    if !status.success() {
        bail!("mkbootimg failed");
    }

    println!("  ✓ cluu.img created");
    Ok(())
}

fn create_user_block_image(_profile: &str) -> Result<()> {
    println!("▸ Creating virtio-blk userspace image...");

    let staging_dir = project_root().join("target/userfs");
    let bin_dir = staging_dir.join("bin");
    let tmp_dir = staging_dir.join("tmp");
    let home_root_dir = staging_dir.join("home/root");
    let var_containers_dir = staging_dir.join("var/containers");
    let var_images_dir = staging_dir.join("var/images");
    let _ = fs::remove_dir_all(&bin_dir);
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&tmp_dir)?;
    fs::create_dir_all(&home_root_dir)?;
    fs::create_dir_all(&var_containers_dir)?;
    fs::create_dir_all(&var_images_dir)?;

    // Copy built container images into /var/images/ on the userdisk
    let containers_dir = project_root().join("target/containers");
    if containers_dir.exists() {
        if let Ok(entries) = fs::read_dir(&containers_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let name = entry.file_name();
                    let dst = var_images_dir.join(&name);
                    fs::create_dir_all(&dst)?;
                    copy_container_image(&entry.path(), &dst).with_context(|| {
                        format!(
                            "Failed to copy container image '{}'",
                            name.to_string_lossy()
                        )
                    })?;
                    println!("  Added container image: {}", name.to_string_lossy());
                }
            }
        }
    }

    // Copy /etc/autostart.toml for procmgr service autostart
    let etc_dir = staging_dir.join("etc");
    fs::create_dir_all(&etc_dir)?;
    let autostart_src = project_root().join("etc/autostart.toml");
    if autostart_src.exists() {
        fs::copy(&autostart_src, etc_dir.join("autostart.toml"))?;
        println!("  Added /etc/autostart.toml");
    }
    let users_src = project_root().join("etc/users.toml");
    if users_src.exists() {
        fs::copy(&users_src, etc_dir.join("users.toml"))?;
        println!("  Added /etc/users.toml");
    }

    let disk_path = project_root().join("target/userdisk.img");
    if disk_path.exists() {
        fs::remove_file(&disk_path)?;
    }

    // Create ext2 image populated from staging_dir using mke2fs -d
    let status = Command::new("mke2fs")
        .args([
            "-t",
            "ext2",
            "-d",
            staging_dir.to_str().unwrap(),
            "-L",
            "cluuuser",
            "-b",
            "1024",
            disk_path.to_str().unwrap(),
            "786432", // 768MB image (786432 blocks * 1KiB)
        ])
        .status()
        .context("Failed to run mke2fs for user disk")?;

    if !status.success() {
        bail!("mke2fs failed while creating user disk image");
    }

    println!("  ✓ userdisk.img created");
    Ok(())
}

fn run_qemu(debug: bool) -> Result<()> {
    let img_path = project_root().join("target/cluu.img");
    let user_disk = project_root().join("target/userdisk.img");

    if !img_path.exists() {
        bail!(
            "Disk image not found at {}. Run 'cargo xtask build' or 'cargo xtask run --build' first.",
            img_path.display()
        );
    }
    if !user_disk.exists() {
        bail!(
            "User disk image not found at {}. Run 'cargo xtask build' or 'cargo xtask run --build' first.",
            user_disk.display()
        );
    }

    // Try to find OVMF.fd
    let ovmf_paths = [
        "/usr/share/ovmf/OVMF.fd",
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/edk2-ovmf/x64/OVMF.fd",
    ];

    let ovmf = ovmf_paths
        .iter()
        .find(|p| PathBuf::from(p).exists())
        .context("OVMF.fd not found. Install ovmf package.")?;

    let mut cmd = Command::new("qemu-system-x86_64");

    // KVM acceleration with host CPU for accurate instruction behavior
    // This exposes real hardware alignment requirements (SSE/AVX)
    cmd.args([
        "-bios",
        ovmf,
        "-m",
        "512M",
        // KVM acceleration (hardware virtualization)
        "-accel",
        "kvm",
        // Use host CPU model for accurate behavior (exposes alignment faults)
        "-cpu",
        "host",
        // Boot disk: force IDE so BOOTBOOT can read it
        "-drive",
        &format!("file={},format=raw,if=ide,index=0", img_path.display()),
        // Data disk: virtio-blk for your driver
        "-drive",
        &format!("file={},format=raw,if=none,id=userblk", user_disk.display()),
        "-device",
        "virtio-blk-pci,drive=userblk",
        "-display",
        "gtk",
        "-no-reboot",
        "-no-shutdown",
    ]);

    if debug {
        println!("▸ Starting QEMU in DEBUG mode...");
        println!("  🔍 GDB server: localhost:1234 (use 'target remote :1234' in gdb)");
        println!("  📡 Telnet serial: localhost:4321 (use 'telnet localhost 4321')");
        println!("  ⏸️  CPU paused - waiting for GDB connection");
        println!("  💡 In GDB: 'continue' to start execution");
        println!();
        println!("  Quick start:");
        println!("    Terminal 1: cargo xtask run --debug");
        println!("    Terminal 2: telnet localhost 4321");
        println!("    Terminal 3: gdb target/x86_64-cluu-kernel/debug/deps/kernel-*.elf");
        println!("                (gdb) target remote :1234");
        println!("                (gdb) continue");
        println!();

        cmd.args([
            "-s", // GDB server on port 1234
            "-S", // Pause CPU at startup
            "-serial",
            "stdio",
            "-serial",
            "telnet:localhost:4321,server,nowait",
        ]);
    } else {
        println!("▸ Starting QEMU...");
        println!("  Press Ctrl+C to exit");
        println!("  Serial output will appear in this terminal");

        cmd.args(["-serial", "stdio"]);
    }

    let status = cmd.status().context("Failed to run QEMU")?;

    if !status.success() {
        bail!("QEMU exited with error");
    }

    // Reset terminal colors
    print!("\x1b[0m");

    Ok(())
}

fn run_tests() -> Result<()> {
    println!("▸ Running tests...");

    // Run host-based unit tests
    let status = Command::new("cargo")
        .current_dir(project_root())
        .args([
            "test",
            "--workspace",
            "--exclude",
            "cluu-init",
            "--exclude",
            "cluu-procmgr",
            "--exclude",
            "cluu-vfs",
            "--exclude",
            "cluu-ramfs",
            "--exclude",
            "cluu-console",
            "--exclude",
            "cluu-shell",
            "--exclude",
            "cluu-cat",
            "--exclude",
            "cluu-virtio-blk",
            "--features",
            "test-mock",
        ])
        .status()
        .context("Failed to run tests")?;

    if !status.success() {
        bail!("Tests failed");
    }

    println!("  ✓ All tests passed");
    Ok(())
}

fn clean() -> Result<()> {
    println!("▸ Cleaning...");

    let _ = Command::new("cargo")
        .current_dir(project_root())
        .args(["clean"])
        .status();

    let _ = fs::remove_dir_all(project_root().join("target/initrd"));
    let _ = fs::remove_file(project_root().join("target/initrd.tar"));
    let _ = fs::remove_file(project_root().join("target/cluu.img"));
    let _ = fs::remove_file(project_root().join("target/userdisk.img"));
    let _ = fs::remove_dir_all(project_root().join("target/asm"));
    let _ = fs::remove_dir_all(project_root().join("target/userfs"));
    let _ = fs::remove_dir_all(project_root().join("userfs"));

    println!("  ✓ Cleaned");
    Ok(())
}

fn clean_full() -> Result<()> {
    println!("▸ Full clean (including toolchain/staging artifacts)...");

    let root = project_root();
    remove_path_if_exists(&root.join("target"))?;
    remove_path_if_exists(&root.join("tmp"))?;
    // Legacy staging location from older builds.
    remove_path_if_exists(&root.join("userfs"))?;
    // Legacy kernel-local artifacts from older build flows.
    remove_path_if_exists(&root.join("kernel/target"))?;
    remove_path_if_exists(&root.join("kernel/cluu-kernel-rust.x86_64.elf"))?;
    remove_path_if_exists(&root.join("kernel/font.o"))?;
    remove_path_if_exists(&root.join("kernel/libfont.a"))?;

    // Downloaded external source trees and archives are build-managed cache.
    // Keep only tracked config in external/.
    if let Ok(sources) = load_external_sources_config() {
        remove_path_if_exists(&sources.newlib_dir)?;
        remove_path_if_exists(&sources.micropython_dir)?;
        if let Some(archive_name) = sources.newlib_url.rsplit('/').next() {
            let archive_path = root.join("external").join(archive_name);
            remove_path_if_exists(&archive_path)?;
        }
    }

    // Clean mkbootimg local build outputs (binary + object files).
    let mkbootimg_dir = root.join("tools/mkbootimg");
    if mkbootimg_dir.exists() {
        let _ = Command::new("make")
            .current_dir(&mkbootimg_dir)
            .arg("clean")
            .status();
    }

    println!("  ✓ Full clean complete");
    Ok(())
}

fn rebuild_full(profile: &str) -> Result<()> {
    println!("▸ Full rebuild from scratch...");
    clean_full()?;

    build_newlib()?;
    build_syscalls(profile)?;
    build_crt0()?;
    build_userspace(profile)?;
    build_kernel(profile)?;
    build_c_programs(profile)?;
    build_micropython()?;
    build_containers()?;
    create_initrd(profile)?;
    create_user_block_image(profile)?;
    create_disk_image(profile)?;

    println!("✓ Full rebuild complete: target/cluu.img");
    Ok(())
}

fn doctor() -> Result<()> {
    println!("▸ CLUU build doctor");
    println!();

    let required_tools = [
        "cargo",
        "rustc",
        "nasm",
        "make",
        "mke2fs",
        "qemu-system-x86_64",
    ];
    let optional_tools = [
        "clang",
        "ld.lld",
        "gdb",
        "telnet",
        "x86_64-linux-gnu-as",
        "x86_64-linux-gnu-ld",
        "x86_64-linux-gnu-gcc",
    ];

    let mut missing_required = Vec::new();

    println!("Required host tools:");
    for tool in required_tools {
        let ok = command_in_path(tool);
        println!("  [{}] {}", if ok { "ok" } else { "missing" }, tool);
        if !ok {
            missing_required.push(tool);
        }
    }

    println!();
    println!("Optional host tools:");
    for tool in optional_tools {
        let ok = command_in_path(tool);
        println!("  [{}] {}", if ok { "ok" } else { "missing" }, tool);
    }

    let root = project_root();
    let sources = load_external_sources_config()?;
    let sysroot = sysroot_path();
    let (newlib_lib, newlib_include) = newlib_paths(&sysroot);
    let newlib_src = sources.newlib_dir.clone();
    let micropython_src = sources.micropython_dir.clone();

    println!();
    println!("Build artifact status:");
    let checks = [
        ("target/cluu.img", root.join("target/cluu.img")),
        ("target/userdisk.img", root.join("target/userdisk.img")),
        (
            "target/sysroot/lib/libcluu_syscalls.a",
            root.join("target/sysroot/lib/libcluu_syscalls.a"),
        ),
        (
            "target/sysroot/lib/crt0.o",
            root.join("target/sysroot/lib/crt0.o"),
        ),
    ];
    for (label, path) in checks {
        println!(
            "  [{}] {}",
            if path.exists() { "ok" } else { "missing" },
            label
        );
    }
    println!(
        "  [{}] {}",
        if newlib_src.exists() { "ok" } else { "missing" },
        relative_to_root_display(&newlib_src)
    );
    println!(
        "  [{}] {}",
        if micropython_src.exists() {
            "ok"
        } else {
            "missing"
        },
        relative_to_root_display(&micropython_src)
    );
    println!(
        "  [{}] {}",
        if newlib_lib.exists() { "ok" } else { "missing" },
        newlib_lib.display()
    );
    println!(
        "  [{}] {}",
        if newlib_include.exists() {
            "ok"
        } else {
            "missing"
        },
        newlib_include.display()
    );

    println!();
    println!("External source config:");
    println!(
        "  [{}] {}",
        if sources.config_path.exists() {
            "ok"
        } else {
            "missing"
        },
        relative_to_root_display(&sources.config_path)
    );
    println!(
        "  newlib: version={} dir={} url={}",
        sources.newlib_version,
        relative_to_root_display(&sources.newlib_dir),
        sources.newlib_url
    );
    println!(
        "  micropython: version={} ref={} dir={}",
        sources.micropython_version,
        sources.micropython_ref.as_deref().unwrap_or("(unset)"),
        relative_to_root_display(&sources.micropython_dir)
    );
    println!("  micropython repo={}", sources.micropython_repo);

    let mut patch_warning = false;
    println!();
    println!("External patch audit:");

    let tracked_newlib = git_ls_files_under(&newlib_src);
    if !tracked_newlib.is_empty() {
        patch_warning = true;
        println!("  [warn] Tracked overrides under newlib source:");
        for entry in tracked_newlib {
            println!("    - {}", entry);
        }
    }

    if !sources.newlib_patch_files.is_empty() {
        patch_warning = true;
        println!("  [warn] Local newlib patch set declared in sources config.");
        println!("  Declared newlib patch files:");
        for rel in &sources.newlib_patch_files {
            let patch_path = newlib_src.join(rel);
            let exists = patch_path.exists();
            if !exists {
                patch_warning = true;
            }
            println!(
                "    - [{}] {}",
                if exists { "present" } else { "missing" },
                relative_to_root_display(&patch_path)
            );
        }
    }

    let tracked_micropython = git_ls_files_under(&micropython_src);
    if !tracked_micropython.is_empty() {
        patch_warning = true;
        println!("  [warn] Tracked overrides under MicroPython source:");
        for entry in tracked_micropython {
            println!("    - {}", entry);
        }
    }

    if !sources.micropython_patch_files.is_empty() {
        patch_warning = true;
        println!("  [warn] Local MicroPython patch set declared in sources config.");
        println!("  Declared MicroPython patch files:");
        for rel in &sources.micropython_patch_files {
            let patch_path = micropython_src.join(rel);
            let exists = patch_path.exists();
            if !exists {
                patch_warning = true;
            }
            println!(
                "    - [{}] {}",
                if exists { "present" } else { "missing" },
                relative_to_root_display(&patch_path)
            );
        }
    }

    if micropython_src.join(".git").exists() {
        if nested_repo_has_tracked_modifications(&micropython_src) {
            patch_warning = true;
            println!("  [warn] MicroPython repo has tracked local modifications.");
        }

        if let Some(current_head) = git_capture_stdout(&micropython_src, &["rev-parse", "HEAD"]) {
            println!("  MicroPython HEAD={}", current_head);
            if let Some(expected_ref) = sources.micropython_ref.as_deref() {
                let expected_expr = format!("{expected_ref}^{{commit}}");
                let expected_commit =
                    git_capture_stdout(&micropython_src, &["rev-parse", expected_expr.as_str()]);
                match expected_commit {
                    Some(expected) if expected != current_head => {
                        patch_warning = true;
                        println!(
                            "  [warn] MicroPython HEAD does not match configured ref '{}' ({})",
                            expected_ref, expected
                        );
                    }
                    Some(_) => {}
                    None => {
                        patch_warning = true;
                        println!(
                            "  [warn] Could not resolve configured MicroPython ref '{}'",
                            expected_ref
                        );
                    }
                }
            }
        }
    } else {
        println!("  [note] MicroPython source is not a nested git repo; ref verification skipped.");
    }

    if patch_warning {
        println!(
            "  [warn] External source patches/deltas detected. Verify patch correctness before lifting versions."
        );
    }

    let legacy_userfs = root.join("userfs");
    if legacy_userfs.exists() {
        println!();
        println!(
            "Note: legacy staging directory exists at {}",
            legacy_userfs.display()
        );
        println!("      Run `cargo xtask clean-full` to remove it.");
    }

    println!();
    if !missing_required.is_empty() {
        bail!(
            "Missing required host tools: {}",
            missing_required.join(", ")
        );
    }
    println!("✓ Doctor check complete");
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("Failed to remove {:?}", path))?;
    } else {
        fs::remove_file(path).with_context(|| format!("Failed to remove {:?}", path))?;
    }
    Ok(())
}

fn command_in_path(tool: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(tool);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

// ============================================================================
// C Toolchain Support
// ============================================================================

fn sysroot_path() -> PathBuf {
    project_root().join("target/sysroot")
}

fn newlib_paths(sysroot: &Path) -> (PathBuf, PathBuf) {
    let cluu_lib = sysroot.join(CLUU_TARGET_TRIPLET).join("lib/libc.a");
    let cluu_include = sysroot.join(CLUU_TARGET_TRIPLET).join("include");
    if cluu_lib.exists() {
        return (cluu_lib, cluu_include);
    }
    let cluu_elf_lib = sysroot.join(NEWLIB_CLUU_TRIPLET).join("lib/libc.a");
    let cluu_elf_include = sysroot.join(NEWLIB_CLUU_TRIPLET).join("include");
    if cluu_elf_lib.exists() {
        return (cluu_elf_lib, cluu_elf_include);
    }
    let newlib_lib = sysroot.join(NEWLIB_TARGET_TRIPLET).join("lib/libc.a");
    let newlib_include = sysroot.join(NEWLIB_TARGET_TRIPLET).join("include");
    (newlib_lib, newlib_include)
}

fn discover_containers() -> Vec<String> {
    let containers_dir = project_root().join("containers");
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(&containers_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let has_cluufile = entry.path().join("Cluufile").exists();
                let has_cargo = entry.path().join("Cargo.toml").exists();
                if has_cluufile || has_cargo {
                    if let Some(name) = entry.file_name().to_str() {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names
}

fn build_klibcluu() -> Result<()> {
    println!("▸ Building klibcluu...");
    let target_json = project_root().join("triplets/x86_64-cluu-kernel.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    let status = Command::new("cargo")
        .current_dir(project_root())
        .args([
            "build",
            "-p",
            "klibcluu",
            "--target",
            target_json.to_str().unwrap(),
            "-Z",
            "build-std=core,alloc",
            "-Z",
            "build-std-features=compiler-builtins-mem",
        ])
        .env("TMPDIR", tmp_dir.as_os_str())
        .status()
        .context("Failed to build klibcluu")?;

    if !status.success() {
        bail!("Failed to build klibcluu");
    }
    println!("  ✓ klibcluu built");
    Ok(())
}

fn build_libcluu(profile: &str) -> Result<()> {
    println!("▸ Building libcluu...");
    let target_json = project_root().join("triplets/x86_64-cluu-user.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root()).args([
        "build",
        "--manifest-path",
        "userspace/libcluu/Cargo.toml",
        "--target",
        target_json.to_str().unwrap(),
        "-Z",
        "build-std=core,alloc",
        "-Z",
        "build-std-features=compiler-builtins-mem",
    ]);
    cmd.env("TMPDIR", tmp_dir.as_os_str());
    if profile == "release" {
        cmd.arg("--release");
    }

    let status = cmd.status().context("Failed to build libcluu")?;
    if !status.success() {
        bail!("Failed to build libcluu");
    }
    println!("  ✓ libcluu built");
    Ok(())
}

/// Init primordial crate names — bootstrapped by init before procmgr takes
/// over.  Everything else (console, kbd, tty, vtmgr, shell) is started by
/// procmgr from containers.
const INIT_CRATES: &[&str] = &[
    "init",
    "registry",
    "timeserver",
    "procmgr",
    "vfs",
    "virtio-blk",
    "tpmd",
];

/// Build a single init primordial crate by name.
fn build_init_crate(name: &str, profile: &str) -> Result<()> {
    println!("▸ Building init crate: {}...", name);

    let crate_path = format!("userspace/{}/Cargo.toml", name);
    let manifest = project_root().join(&crate_path);
    if !manifest.exists() {
        bail!("Init crate manifest not found: {}", manifest.display());
    }

    let target_json = project_root().join("triplets/x86_64-cluu-user.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root()).args([
        "build",
        "--manifest-path",
        manifest.to_str().unwrap(),
        "--target",
        target_json.to_str().unwrap(),
        "-Z",
        "build-std=core,alloc",
        "-Z",
        "build-std-features=compiler-builtins-mem",
    ]);
    cmd.env("TMPDIR", tmp_dir.as_os_str());
    if profile == "release" {
        cmd.arg("--release");
    }

    let status = cmd.status().context("Failed to run cargo")?;
    if !status.success() {
        bail!("Failed to build init crate {}", name);
    }

    println!("  ✓ {} built", name);
    Ok(())
}

fn build_single_container(name: &str) -> Result<()> {
    println!("▸ Building container: {}...", name);
    let containers_dir = project_root().join("containers").join(name);

    // Auto-discover: if Cargo.toml exists in the container dir, build the Rust crate
    let cargo_toml = containers_dir.join("Cargo.toml");
    if cargo_toml.exists() {
        let target_json = project_root().join("triplets/x86_64-cluu-user.json");
        let tmp_dir = project_root().join("tmp");
        fs::create_dir_all(&tmp_dir)?;

        let status = Command::new("cargo")
            .current_dir(project_root())
            .args([
                "build",
                "--manifest-path",
                cargo_toml.to_str().unwrap(),
                "--target",
                target_json.to_str().unwrap(),
                "-Z",
                "build-std=core,alloc",
                "-Z",
                "build-std-features=compiler-builtins-mem",
            ])
            .env("TMPDIR", tmp_dir.as_os_str())
            .status()
            .context("Failed to build container crate")?;

        if !status.success() {
            bail!("Failed to build container crate for {}", name);
        }
    }

    // Run container-build for metadata/packaging via Cluufile
    let cluufile = containers_dir.join("Cluufile");
    if cluufile.exists() {
        let status = Command::new("cargo")
            .args(["run", "-p", "container-build", "--"])
            .arg(&cluufile)
            .status()
            .with_context(|| format!("Failed to run container-build for {}", name))?;
        if !status.success() {
            bail!(
                "container-build failed for {} (exit {:?})",
                name,
                status.code()
            );
        }
    } else if !cargo_toml.exists() {
        bail!("Container {} has neither Cluufile nor Cargo.toml", name);
    }

    println!("  ✓ Container {} built", name);
    Ok(())
}

fn build_newlib() -> Result<()> {
    println!("▸ Building newlib...");
    ensure_newlib_source()?;

    let script = project_root().join("scripts/build-newlib.sh");
    if !script.exists() {
        bail!("build-newlib.sh not found. Run from repository root.");
    }

    let status = Command::new("bash")
        .current_dir(project_root())
        .arg(&script)
        .status()
        .context("Failed to run build-newlib.sh")?;

    if !status.success() {
        bail!("Newlib build failed");
    }

    println!("  ✓ Newlib built");
    Ok(())
}

fn build_syscalls(profile: &str) -> Result<()> {
    println!("▸ Building libcluu_syscalls...");

    let target_json = project_root().join("triplets/x86_64-cluu-user.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root()).args([
        "build",
        "-p",
        "libcluu_syscalls",
        "--target",
        target_json.to_str().unwrap(),
        "-Z",
        "build-std=core,alloc",
        "-Z",
        "build-std-features=compiler-builtins-mem",
    ]);
    cmd.env("TMPDIR", tmp_dir.as_os_str());

    if profile == "release" {
        cmd.arg("--release");
    }

    let status = cmd.status().context("Failed to build libcluu_syscalls")?;
    if !status.success() {
        bail!("Failed to build libcluu_syscalls");
    }

    // Copy to sysroot
    let cargo_profile = if profile == "dev" { "debug" } else { profile };
    let src = project_root()
        .join("target/x86_64-cluu-user")
        .join(cargo_profile)
        .join("libcluu_syscalls.a");
    let sysroot = sysroot_path();
    fs::create_dir_all(sysroot.join("lib"))?;
    let dst = sysroot.join("lib/libcluu_syscalls.a");
    fs::copy(&src, &dst).context("Failed to copy libcluu_syscalls.a to sysroot")?;

    println!("  ✓ libcluu_syscalls.a installed to {}", dst.display());
    Ok(())
}

fn build_crt0() -> Result<()> {
    println!("▸ Assembling crt0.o...");

    let crt0_src = project_root().join("userspace/newlib/crt0.S");
    if !crt0_src.exists() {
        bail!("crt0.S not found at {:?}", crt0_src);
    }

    let sysroot = sysroot_path();
    fs::create_dir_all(sysroot.join("lib"))?;
    let crt0_dst = sysroot.join("lib/crt0.o");

    // Try clang first, fall back to GCC
    let status = Command::new("clang")
        .args([
            &format!("--target={}", CLUU_CLANG_TARGET),
            "-c",
            "-o",
            crt0_dst.to_str().unwrap(),
            crt0_src.to_str().unwrap(),
        ])
        .status();

    let success = match status {
        Ok(s) if s.success() => true,
        _ => {
            // Fall back to GNU assembler
            println!("  clang not found, trying x86_64-linux-gnu-as...");
            let status = Command::new("x86_64-linux-gnu-as")
                .args(["-o", crt0_dst.to_str().unwrap(), crt0_src.to_str().unwrap()])
                .status()
                .context("Failed to run assembler")?;
            status.success()
        }
    };

    if !success {
        bail!("Failed to assemble crt0.o");
    }

    println!("  ✓ crt0.o installed to {}", crt0_dst.display());
    Ok(())
}

/// Build all C programs in userspace/c-programs etc.
/// This builds prerequisites (syscalls, crt0, newlib) if needed, then compiles C programs.
fn build_c_programs(profile: &str) -> Result<()> {
    let sysroot = sysroot_path();
    let crt0 = sysroot.join("lib/crt0.o");
    let syscalls = sysroot.join("lib/libcluu_syscalls.a");

    // Check if libcluu source is newer than syscalls.a (staleness detection)
    let syscalls_stale = is_syscalls_stale(&syscalls);

    // Build prerequisites if missing or stale
    if !syscalls.exists() || syscalls_stale {
        if syscalls_stale {
            println!("▸ Rebuilding libcluu_syscalls.a (source changed)...");
        } else {
            println!("▸ Building libcluu_syscalls.a (prerequisite for C programs)...");
        }
        build_syscalls(profile)?;
    }
    if !crt0.exists() {
        println!("▸ Building crt0.o (prerequisite for C programs)...");
        build_crt0()?;
    }

    // Check for newlib and build/install if needed
    let (newlib_lib, _) = newlib_paths(&sysroot);
    if !newlib_lib.exists() {
        println!("▸ Installing newlib to sysroot (required for C programs)...");
        ensure_newlib_installed()?;
    }

    // List of C programs to build: (name, source_path)
    let c_programs: &[(&str, &str)] = &[
        ("hello", "userspace/c-programs/hello.c"),
        ("minimal", "userspace/c-programs/minimal.c"),
        ("noop", "userspace/c-programs/noop.c"),
        ("ownerprobe", "userspace/c-programs/ownerprobe.c"),
        ("sleepy", "userspace/c-programs/sleepy.c"),
        ("waitprobe", "userspace/c-programs/waitprobe.c"),
        ("mmapprobe", "userspace/c-programs/mmapprobe.c"),
        ("pollprobe", "userspace/c-programs/pollprobe.c"),
        ("benchprobe", "userspace/c-programs/benchprobe.c"),
        ("futexprobe", "userspace/c-programs/futexprobe.c"),
        ("futexrace", "userspace/c-programs/futexrace.c"),
        ("setjmpprobe", "userspace/c-programs/setjmpprobe.c"),
        ("envprobe", "userspace/c-programs/envprobe.c"),
        ("stubsprobe", "userspace/c-programs/stubsprobe.c"),
        ("pipeprobe", "userspace/c-programs/pipeprobe.c"),
        ("pipecat", "userspace/c-programs/pipecat.c"),
        ("spawnpipeprobe", "userspace/c-programs/spawnpipeprobe.c"),
        ("tlsprobe", "userspace/c-programs/tlsprobe.c"),
        ("pthreadprobe", "userspace/c-programs/pthreadprobe.c"),
        ("devprobe", "userspace/c-programs/devprobe.c"),
        ("fbprobe", "userspace/c-programs/fbprobe.c"),
        ("containerprobe", "userspace/c-programs/containerprobe.c"),
    ];

    for (name, source) in c_programs {
        let source_path = project_root().join(source);
        if source_path.exists() {
            build_c_program(name, &source_path, profile)?;
        } else {
            println!("  Skipping {} (source not found)", name);
        }
    }

    Ok(())
}

/// Build MicroPython port (if source exists)
fn build_micropython() -> Result<()> {
    let micropython_dir = project_root().join("userspace/micropython");
    if !micropython_dir.exists() {
        println!("▸ MicroPython port directory not found, skipping");
        return Ok(());
    }

    if let Err(err) = ensure_micropython_source() {
        eprintln!(
            "  Warning: failed to fetch/clone MicroPython sources; skipping MicroPython build ({err:#})"
        );
        return Ok(());
    }

    let sources = match load_external_sources_config() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!(
                "  Warning: failed to load external source config for MicroPython; skipping ({err:#})"
            );
            return Ok(());
        }
    };

    let micropython_src = sources.micropython_dir.join("py/py.mk");
    if !micropython_src.exists() {
        eprintln!(
            "  Warning: MicroPython source missing at {} after fetch; skipping MicroPython build",
            micropython_src.display()
        );
        return Ok(());
    }

    println!("▸ Building MicroPython {}...", sources.micropython_version);
    let status = Command::new("make")
        .current_dir(&micropython_dir)
        .arg("-j4")
        .status()
        .context("Failed to build MicroPython")?;
    if !status.success() {
        bail!("MicroPython build failed");
    }
    println!("  ✓ MicroPython built");
    Ok(())
}

/// Check if libcluu_syscalls.a is stale (source newer than artifact)
fn is_syscalls_stale(syscalls_path: &Path) -> bool {
    if !syscalls_path.exists() {
        return false; // Not stale, just missing
    }

    let syscalls_mtime = match fs::metadata(syscalls_path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    // Check if any libcluu source file is newer
    let libcluu_src = project_root().join("userspace/libcluu/src");
    if let Ok(entries) = fs::read_dir(&libcluu_src) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if mtime > syscalls_mtime {
                        return true;
                    }
                }
            }
        }
    }

    // Also check boot.rs specifically (token constants live here)
    let boot_rs = libcluu_src.join("boot.rs");
    if let Ok(meta) = fs::metadata(&boot_rs) {
        if let Ok(mtime) = meta.modified() {
            if mtime > syscalls_mtime {
                return true;
            }
        }
    }

    false
}

/// Ensure newlib is built and installed to sysroot
fn ensure_newlib_installed() -> Result<()> {
    let sysroot = sysroot_path();
    let (newlib_lib, _) = newlib_paths(&sysroot);

    if newlib_lib.exists() {
        return Ok(());
    }

    // Check if newlib was built but not installed
    let build_dir = project_root().join("target/newlib-build");
    let built_libc = build_dir.join(NEWLIB_CLUU_TRIPLET).join("newlib/libc.a");

    if built_libc.exists() {
        // Newlib was built, just need to install
        println!("  Running make install for newlib...");
        let status = Command::new("make")
            .current_dir(&build_dir)
            .arg("install")
            .status()
            .context("Failed to run make install for newlib")?;

        if !status.success() {
            bail!("Newlib install failed");
        }

        // Verify installation
        let (newlib_lib_after, _) = newlib_paths(&sysroot);
        if !newlib_lib_after.exists() {
            bail!("Newlib install completed but libc.a not found in sysroot");
        }

        println!("  ✓ Newlib installed to sysroot");
    } else {
        // Need full build
        build_newlib()?;
    }

    Ok(())
}

fn build_c_program(name: &str, source: &Path, profile: &str) -> Result<()> {
    println!("▸ Building C program: {}", name);

    let sysroot = sysroot_path();
    let crt0 = sysroot.join("lib/crt0.o");
    let syscalls = sysroot.join("lib/libcluu_syscalls.a");

    // Check prerequisites
    if !crt0.exists() {
        bail!("crt0.o not found. Run 'cargo xtask build-crt0' first.");
    }
    if !syscalls.exists() {
        bail!("libcluu_syscalls.a not found. Run 'cargo xtask build-syscalls' first.");
    }
    if !source.exists() {
        bail!("Source file not found: {:?}", source);
    }

    let cargo_profile = if profile == "dev" { "debug" } else { profile };
    // Honour CLUU_BUILD_OUTPUT_DIR so container-build can redirect outputs
    // to a container-scoped directory, avoiding races in parallel builds.
    let out_dir = match std::env::var_os("CLUU_BUILD_OUTPUT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => project_root()
            .join("target/x86_64-cluu-user")
            .join(cargo_profile),
    };
    fs::create_dir_all(&out_dir)?;

    let obj_file = out_dir.join(format!("{}.o", name));
    let elf_file = out_dir.join(format!("{}.elf", name));
    let linker_script = project_root().join("userspace/user.ld");

    // Check for newlib in sysroot
    let (newlib_lib, newlib_include) = newlib_paths(&sysroot);
    let have_newlib = newlib_lib.exists();
    let newlib_lib_dir = newlib_lib.parent();

    // Compile - try clang first, fall back to GCC
    println!("  Compiling {}...", source.display());

    let compile_success = {
        // Try clang first
        let mut compile_cmd = Command::new("clang");
        compile_cmd.args([
            &format!("--target={}", CLUU_CLANG_TARGET),
            "-ffreestanding",
            "-fno-stack-protector",
            "-nostdlib",
            "-c",
        ]);

        if have_newlib {
            compile_cmd.arg("-I").arg(newlib_include.to_str().unwrap());
        }

        compile_cmd.args(["-o", obj_file.to_str().unwrap(), source.to_str().unwrap()]);

        match compile_cmd.status() {
            Ok(s) if s.success() => true,
            _ => {
                // Fall back to GCC
                println!("  clang not found, trying x86_64-linux-gnu-gcc...");
                let mut gcc_cmd = Command::new("x86_64-linux-gnu-gcc");
                gcc_cmd.args([
                    "-ffreestanding",
                    "-fno-stack-protector",
                    "-nostdlib",
                    "-mno-red-zone",
                    "-c",
                ]);

                if have_newlib {
                    gcc_cmd.arg("-I").arg(newlib_include.to_str().unwrap());
                }

                gcc_cmd.args(["-o", obj_file.to_str().unwrap(), source.to_str().unwrap()]);

                match gcc_cmd.status() {
                    Ok(s) => s.success(),
                    Err(e) => {
                        eprintln!("  Failed to run compiler: {}", e);
                        false
                    }
                }
            }
        }
    };

    if !compile_success {
        bail!("Compilation failed. Install clang or x86_64-linux-gnu-gcc.");
    }

    // Link - try ld.lld first, fall back to ld
    println!("  Linking {}...", name);

    let link_success = {
        let mut link_cmd = Command::new("ld.lld");
        link_cmd.args([
            "-T",
            linker_script.to_str().unwrap(),
            "-o",
            elf_file.to_str().unwrap(),
            crt0.to_str().unwrap(),
            obj_file.to_str().unwrap(),
            "-L",
            sysroot.join("lib").to_str().unwrap(),
            "-lcluu_syscalls",
        ]);

        if have_newlib {
            if let Some(dir) = newlib_lib_dir {
                link_cmd.arg("-L").arg(dir.to_str().unwrap());
            }
            link_cmd.args(["-lc", "-lm"]);
        }

        match link_cmd.status() {
            Ok(s) if s.success() => true,
            _ => {
                // Fall back to GNU ld
                println!("  ld.lld not found, trying x86_64-linux-gnu-ld...");
                let mut ld_cmd = Command::new("x86_64-linux-gnu-ld");
                ld_cmd.args([
                    "-T",
                    linker_script.to_str().unwrap(),
                    "-o",
                    elf_file.to_str().unwrap(),
                    crt0.to_str().unwrap(),
                    obj_file.to_str().unwrap(),
                    "-L",
                    sysroot.join("lib").to_str().unwrap(),
                    "-lcluu_syscalls",
                ]);

                if have_newlib {
                    if let Some(dir) = newlib_lib_dir {
                        ld_cmd.arg("-L").arg(dir.to_str().unwrap());
                    }
                    ld_cmd.args(["-lc", "-lm"]);
                }

                match ld_cmd.status() {
                    Ok(s) => s.success(),
                    Err(e) => {
                        eprintln!("  Failed to run linker: {}", e);
                        false
                    }
                }
            }
        }
    };

    if !link_success {
        if !have_newlib {
            bail!(
                "Linking failed - newlib not found in sysroot.\n\
                 C programs using printf/malloc/etc require newlib.\n\
                 Run: ./scripts/download-newlib.sh && cargo xtask build-newlib"
            );
        }
        bail!("Linking failed. Install lld or x86_64-linux-gnu-ld.");
    }

    println!("  ✓ Built: {}", elf_file.display());
    Ok(())
}

fn setup_c_toolchain() -> Result<()> {
    println!("▸ Setting up C toolchain for CLUU...");

    // Step 1: Build syscalls library
    build_syscalls("dev")?;

    // Step 2: Build crt0.o
    build_crt0()?;

    // Step 3: Build newlib (auto-download if missing)
    println!();
    build_newlib()?;

    println!();
    println!("✓ C toolchain setup complete!");
    println!();
    println!("Sysroot: {}", sysroot_path().display());
    println!();
    println!("To build a C program:");
    println!("  cargo xtask build-c <name> <source.c>");
    println!();
    println!("Example:");
    println!("  cargo xtask build-c hello userspace/c-programs/hello.c");

    Ok(())
}

/// Recursively copy a container image directory tree.
fn build_containers() -> Result<()> {
    println!("▸ Building container images...");

    let containers_dir = project_root().join("containers");
    if !containers_dir.exists() {
        println!("  No containers/ directory found, skipping");
        return Ok(());
    }

    let mut cluufiles: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&containers_dir)
        .with_context(|| format!("Failed to read {}", containers_dir.display()))?
    {
        let entry = entry?;
        if entry.path().is_dir() {
            let cluufile = entry.path().join("Cluufile");
            if cluufile.exists() {
                cluufiles.push(cluufile);
            }
        }
    }
    cluufiles.sort();

    if cluufiles.is_empty() {
        println!("  No Cluufiles found in containers/*/");
        return Ok(());
    }

    for cluufile in &cluufiles {
        let name = cluufile
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("  Building container: {}", name);
        let status = Command::new("cargo")
            .args(["run", "-p", "container-build", "--"])
            .arg(cluufile)
            .status()
            .with_context(|| format!("Failed to run container-build for {}", name))?;
        if !status.success() {
            bail!(
                "container-build failed for {} (exit {:?})",
                name,
                status.code()
            );
        }
    }

    println!("  ✓ {} container image(s) built", cluufiles.len());
    Ok(())
}

fn copy_container_image(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst_dir.join(entry.file_name());
        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            copy_container_image(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
