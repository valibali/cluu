//! Build orchestration for CLUU
//!
//! Usage:
//!   cargo xtask build          # Build everything
//!   cargo xtask run            # Run existing disk image in QEMU
//!   cargo xtask run --build    # Build and then run in QEMU
//!   cargo xtask test           # Run all tests
//!   cargo xtask clean          # Clean all build artifacts

mod tui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir;

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
        /// Pin QEMU to a specific host CPU core (e.g. --pin-core 3)
        #[arg(long)]
        pin_core: Option<usize>,
        /// Start a host HTTP server on port 9876 for virtio-net demos
        /// (guest can curl 10.0.2.2:9876). Use --no-net to disable.
        #[arg(long, default_value_t = true)]
        net: bool,
        /// Override the host HTTP server port (default: 9876)
        #[arg(long, default_value_t = 9876)]
        port: u16,
        /// QEMU display backend: gtk (window) or none (headless)
        #[arg(long, default_value = "gtk")]
        display: String,
        /// Use virtio-gpu-pci instead of default VGA (adds -vga none
        /// -device virtio-gpu-pci,max_outputs=1,edid=on)
        #[arg(long)]
        virtio_gpu: bool,
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
    /// Internal: build doom port
    #[command(hide = true)]
    BuildDoom,
    /// Internal: build pinned SDL2 2.30.0 static library for CLUU
    #[command(hide = true)]
    BuildSdl2,
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
    /// Check procmgr crates for forbidden ACL-style identifiers
    CheckCapPurity,
    /// Run cargo-llvm-cov on procmgr crates and enforce 95% line+branch threshold
    CoverageCheck {
        /// Skip the gate (just report coverage) — useful while ratcheting up.
        #[arg(long)]
        report_only: bool,
        /// Override default threshold (percent, 0-100).
        #[arg(long, default_value_t = 95.0)]
        threshold: f64,
    },
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
            pin_core,
            net,
            port,
            display,
            virtio_gpu,
        } => {
            if build {
                build_pipeline(&profile, ui)?;
            }
            run_qemu(debug, pin_core, net, port, &display, virtio_gpu)?;
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
        Commands::BuildDoom => {
            build_doom()?;
        }
        Commands::BuildSdl2 => {
            build_sdl2()?;
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
        Commands::CoverageCheck {
            report_only,
            threshold,
        } => {
            coverage_check(report_only, threshold)?;
        }
        Commands::CheckCapPurity => {
            check_cap_purity()?;
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
    let _ = build_containers();
    // Packaging
    create_initrd(profile)?;
    create_user_block_image(profile)?;
    create_disk_image(profile)?;
    Ok(())
}

fn logs_root_dir() -> PathBuf {
    project_root().join("target").join("logs")
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

// RichTreeNodeDef re-exported from tui module for build_pipeline_rich.
// The old RichTreeUi/render_tree_frame has been replaced by ratatui TUI in tui.rs.

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

fn run_rich_task(
    task: &RichTask,
    logs_dir: &Path,
    tui_state: Option<Arc<Mutex<tui::TuiState>>>,
) -> Result<()> {
    let task_name = task.name.clone();
    let log_path = logs_dir.join(format!("{}.log", task_name));
    if let Some(ref state) = tui_state {
        state
            .lock()
            .expect("tui state lock poisoned")
            .start_task(&task_name);
    }

    let sink: TaskSink = if let Some(ref state) = tui_state {
        let name_for_sink = task_name.clone();
        let state = Arc::clone(state);
        Arc::new(move |line: String| {
            state
                .lock()
                .expect("tui state lock poisoned")
                .push_line(&name_for_sink, line);
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
            if let Some(ref state) = tui_state {
                state
                    .lock()
                    .expect("tui state lock poisoned")
                    .finish_task(&task_name, false, None);
            }
            Ok(())
        }
        Err(err) => {
            if let Some(ref state) = tui_state {
                let fail_log = Some(relative_to_root_display(&log_path));
                state
                    .lock()
                    .expect("tui state lock poisoned")
                    .finish_task(&task_name, true, fail_log);
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
    tree_defs: Vec<tui::TreeNodeDef>,
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

    let tui_state = if interactive_tree {
        Some(Arc::new(Mutex::new(tui::TuiState::new(
            "CLUU rich build".to_string(),
            logs_dir.to_path_buf(),
            &tree_defs,
        ))))
    } else {
        None
    };

    let renderer = if let Some(ref state) = tui_state {
        let state = Arc::clone(state);
        Some(thread::spawn(move || {
            let _ = tui::run_tui(state);
        }))
    } else {
        None
    };

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
                let tui_state = tui_state.clone();

                thread::spawn(move || {
                    let name = task.name.clone();
                    let result = run_rich_task(&task, &logs_dir, tui_state);
                    let _ = tx.send((name, result));
                });
            }
        }

        if running.is_empty() {
            if let Some(ref state) = tui_state {
                state.lock().expect("tui state lock poisoned").stop = true;
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
        tui::TreeNodeDef {
            id: "build".into(),
            label: "build".into(),
            parent: None,
            is_leaf: false,
        },
        // Dependencies
        tui::TreeNodeDef {
            id: "dependencies".into(),
            label: "dependencies".into(),
            parent: Some("build".into()),
            is_leaf: false,
        },
        tui::TreeNodeDef {
            id: "dep-klibcluu".into(),
            label: "klibcluu".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        tui::TreeNodeDef {
            id: "dep-libcluu".into(),
            label: "libcluu".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        tui::TreeNodeDef {
            id: "dep-newlib".into(),
            label: "newlib".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        tui::TreeNodeDef {
            id: "dep-syscalls".into(),
            label: "syscalls".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        tui::TreeNodeDef {
            id: "dep-crt0".into(),
            label: "crt0".into(),
            parent: Some("dependencies".into()),
            is_leaf: true,
        },
        // Kernel
        tui::TreeNodeDef {
            id: "kernel".into(),
            label: "kernel".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
        // Userspace
        tui::TreeNodeDef {
            id: "userspace".into(),
            label: "userspace".into(),
            parent: Some("build".into()),
            is_leaf: false,
        },
        tui::TreeNodeDef {
            id: "init".into(),
            label: "init".into(),
            parent: Some("userspace".into()),
            is_leaf: false,
        },
        tui::TreeNodeDef {
            id: "containers".into(),
            label: "containers".into(),
            parent: Some("userspace".into()),
            is_leaf: false,
        },
        // Packaging
        tui::TreeNodeDef {
            id: "initrd".into(),
            label: "initrd".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
        tui::TreeNodeDef {
            id: "userdisk".into(),
            label: "userdisk".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
        tui::TreeNodeDef {
            id: "disk-image".into(),
            label: "disk-image".into(),
            parent: Some("build".into()),
            is_leaf: true,
        },
    ];

    // Init primordial subtasks (one per crate)
    for crate_name in INIT_CRATES {
        tree_defs.push(tui::TreeNodeDef {
            id: format!("init-{}", crate_name),
            label: crate_name.to_string(),
            parent: Some("init".into()),
            is_leaf: true,
        });
    }

    // Dynamic container nodes (auto-discovered from containers/*/)
    for name in &container_names {
        tree_defs.push(tui::TreeNodeDef {
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

    let tui_capable = tui::is_tui_capable();
    run_rich_dag(tasks, tree_defs, &logs_dir, tui_capable)?;

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
        "userspace/virtio-net", // Network driver
        "userspace/virtio-9p",  // 9p host share driver
        "userspace/dma-core",   // DMA library
        "userspace/xhci-core",  // xHCI driver library
        "userspace/usb-hid",    // USB HID library
        "userspace/ext2",       // Filesystem library
        "userspace/init",       // System programs
        "userspace/root-procmgr",
        "userspace/registry",
        "userspace/vfs",
        "userspace/ramfs",
        "userspace/compositor",
        "userspace/compdemo",
        "userspace/displayd",
        "userspace/audiod",
        "userspace/console",
        "userspace/kbd",
        "userspace/tty",
        "userspace/vtmgr",
        "userspace/shell",
        "userspace/timeserver",
        "userspace/devmgr",
        "userspace/cat",
        "userspace/grep",
        "userspace/head",
        "userspace/tail",
        "userspace/wc",
        "userspace/ls",
        "userspace/ps",
        "userspace/touch",
        "userspace/tpmd",
        "userspace/usb-input",
        "userspace/probes/dynprobe",
        "userspace/basename",
        "userspace/date",
        "userspace/dirname",
        "userspace/env",
        "userspace/kill",
        "userspace/printf",
        "userspace/sleep",
        "userspace/which",
        "userspace/sort",
        "userspace/uniq",
        "userspace/cut",
        "userspace/tr",
        "userspace/find",
        "userspace/du",
        "userspace/login",
        "userspace/stat",
        "userspace/cluuterm",
        "userspace/netd",
        "userspace/ping",
        "userspace/wget",
        "userspace/curl",
        "userspace/probes/l2_socket_basic",
        "userspace/probes/l2_net_denied",
        "userspace/probes/l2_dns_basic",
        "userspace/mail",
        "userspace/feed",
        "userspace/notes",
        "userspace/glow",
        "userspace/sysmon",
        "userspace/pkg",
        "userspace/libtui",
        "userspace/libtui-demo",
        "userspace/fm",
        "userspace/pager",
        "userspace/hexdump",
        "userspace/calc",
        "userspace/diff",
        "userspace/irc",
        "userspace/httpd",
        "userspace/ntp",
        "userspace/git",
        "userspace/sed",
        "userspace/awk",
        "userspace/make",
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
        cmd.env("CC_x86_64_cluu_user", "clang");

        if profile == "release" {
            cmd.arg("--release");
        }

        // libtui gates its runtime (StdinReader/Renderer/Program) behind
        // the "runtime" feature, which pulls in libcluu. Pure-logic types
        // (Model/Cmd/View/input decoder) are always available.
        if *crate_path == "userspace/libtui" {
            cmd.args(["--features", "runtime"]);
        }

        // Baseline instrumentation: enable bench probes in the compositor
        // and displayd when CLUU_BENCH=1 is set. Probes are compile-time
        // gated so non-baseline builds have zero overhead.
        if std::env::var("CLUU_BENCH").as_deref() == Ok("1")
            && (*crate_path == "userspace/compositor" || *crate_path == "userspace/displayd")
        {
            cmd.args(["--features", "bench"]);
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

    // Opt-in kernel trace logging (LogLevel::Trace). Required for diagnostic
    // builds that need klibcluu::trace / log_dec(Trace, ...) output on COM2.
    // Without this, Trace-level calls compile to no-ops even in debug builds.
    if std::env::var("CLUU_LOG_TRACE").as_deref() == Ok("1") {
        println!("  CLUU_LOG_TRACE=1: enabling kernel log-trace feature");
        cmd.args(["--features", "log-trace"]);
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
        // Strip debug sections to reduce initrd size — the full debug ELF
        // remains in target/ for GDB. Same treatment as userspace binaries
        // below. Without this the kernel is ~14 MB with debug info vs ~3 MB
        // stripped, which can exceed BOOTBOOT's decompression buffer.
        let strip_result = Command::new("strip")
            .args(["--strip-debug", "-o"])
            .arg(&dst)
            .arg(kernel_entry.path())
            .status();
        match strip_result {
            Ok(s) if s.success() => {
                println!("  Copied kernel as sys/core (stripped)");
            }
            _ => {
                // Fall back to plain copy if strip is unavailable.
                fs::copy(kernel_entry.path(), &dst).context("Failed to copy kernel as sys/core")?;
                println!("  Copied kernel as sys/core");
            }
        }
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
        "devmgr",
        "root-procmgr",
        "vfs",
        "drivermgr",
        "drivermon",
        "virtio-blk",
        "virtio-net",
        "virtio-9p",
        "virtio-snd",
        "virtio-gpu",
        "netd",
        "tpmd",
        "usb-input",
        "kbd",
        "mouse",
    ];
    let mut copied_sys_paths = Vec::new();
    for prog in &sys_programs {
        let src = userspace_target_dir.join(format!("{}.elf", prog));
        let dst = initrd_dir.join("sys").join(prog);
        if src.exists() {
            // Strip debug sections to reduce initrd size — the full debug
            // ELF remains in target/ for GDB. Debug info in the initrd
            // is dead weight (initrd is gzip-compressed at the disk-image
            // level, but the uncompressed size still matters for the
            // BOOTBOOT decompression buffer).
            let strip_result = Command::new("strip")
                .args(["--strip-debug", "-o"])
                .arg(&dst)
                .arg(&src)
                .status();
            match strip_result {
                Ok(s) if s.success() => {
                    println!("  Copied sys/{} (stripped)", prog);
                }
                _ => {
                    // Fall back to plain copy if llvm-strip is unavailable.
                    fs::copy(&src, &dst).with_context(|| format!("Failed to copy {}", prog))?;
                    println!("  Copied sys/{}", prog);
                }
            }
            copied_sys_paths.push(format!("sys/{}", prog));
        } else {
            bail!("Required system binary '{}' not found at {:?}", prog, src);
        }
    }

    // Note: shell and other userspace programs are loaded from ext2 containers,
    // not from the initrd.

    // D3.6: Copy driver manifests and .elf extensions to initrd for two-phase
    // boot. In spawn mode, drivermgr reads manifests directly from initrd
    // (VFS is blocked until blkdev registers), so driver manifests must be
    // in the initrd archive alongside the driver binaries.
    let driver_programs = [
        "virtio-blk",
        "virtio-net",
        "virtio-9p",
        "virtio-snd",
        "virtio-gpu",
        "usb-input",
        "kbd",
        "mouse",
    ];
    for prog in &driver_programs {
        let stripped = initrd_dir.join("sys").join(prog);
        let dst_elf = initrd_dir.join("sys").join(format!("{}.elf", prog));
        if stripped.exists() {
            fs::copy(&stripped, &dst_elf).with_context(|| {
                format!("Failed to copy {}.elf", prog)
            })?;
            println!("  Copied sys/{}.elf", prog);
        }
    }

    let containers_dir = project_root().join("target/containers");
    if containers_dir.exists() {
        for prog in &driver_programs {
            let manifest_src = containers_dir.join(prog).join("manifest.toml");
            if !manifest_src.exists() {
                continue;
            }
            let content = match fs::read_to_string(&manifest_src) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !content.contains("[driver]") {
                continue;
            }
            let dst = initrd_dir.join("sys").join(format!("{}.manifest.toml", prog));
            fs::copy(&manifest_src, &dst).with_context(|| {
                format!("Failed to copy {}.manifest.toml", prog)
            })?;
            println!("  Copied sys/{}.manifest.toml", prog);
        }
    }

    let drivermgr_toml_src = project_root().join("etc/drivermgr.toml");
    if drivermgr_toml_src.exists() {
        fs::copy(&drivermgr_toml_src, initrd_dir.join("etc/drivermgr.toml"))?;
        println!("  Copied etc/drivermgr.toml");
    }

    let system_toml_initrd_src = project_root().join("etc/system.toml");
    if system_toml_initrd_src.exists() {
        fs::copy(&system_toml_initrd_src, initrd_dir.join("etc/system.toml"))?;
        println!("  Copied etc/system.toml");
    }

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
        // Note: binary was renamed procmgr → root-procmgr in 1674f98.
        "sys/root-procmgr" => {
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
                | RIGHT_PCI_ACCESS
                | RIGHT_GRANT
        }
        "sys/virtio-blk" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
        }
        "sys/virtio-net" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
        }
        "sys/virtio-9p" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
        }
        "sys/virtio-snd" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
        }
        "sys/virtio-gpu" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
        }
        "sys/usb-input" => {
            RIGHT_PCI_ACCESS
                | RIGHT_SPACE_MAP
                | RIGHT_IPC_SEND
                | RIGHT_IPC_RECV
                | RIGHT_CREATE
                | RIGHT_GRANT
                | RIGHT_IRQ_HANDLE
                | RIGHT_IRQ_ACK
        }
        "sys/tpmd" => RIGHT_SPACE_MAP | RIGHT_IPC_SEND | RIGHT_IPC_RECV | RIGHT_CREATE,
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
        String::from("// BOOTBOOT configuration\nscreen=1920x1080\nkernel=sys/core\n");
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
    let lib_dir = staging_dir.join("lib");
    let usr_dir = staging_dir.join("usr");
    let tmp_dir = staging_dir.join("tmp");
    let home_root_dir = staging_dir.join("home/root");
    let var_containers_dir = staging_dir.join("var/containers");
    let var_images_dir = staging_dir.join("var/images");
    let var_log_dir = staging_dir.join("var/log");
    let _ = fs::remove_dir_all(&bin_dir);
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&lib_dir)?;
    fs::create_dir_all(&usr_dir)?;
    fs::create_dir_all(&tmp_dir)?;
    fs::create_dir_all(&home_root_dir)?;
    fs::create_dir_all(&var_containers_dir)?;
    fs::create_dir_all(&var_images_dir)?;
    fs::create_dir_all(&var_log_dir)?;

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

    // Populate /bin with symlinks → /var/images/<image>/bin/<bin>. Each
    // container's bin/ may hold one or more executables; expose every one
    // under /bin so that PATH-based and full-path lookups (e.g. /bin/ls)
    // both resolve through ext2 symlink following.
    if var_images_dir.exists() {
        if let Ok(image_entries) = fs::read_dir(&var_images_dir) {
            for image_entry in image_entries.flatten() {
                let image_path = image_entry.path();
                if !image_path.is_dir() {
                    continue;
                }
                let image_name = image_entry.file_name();
                let image_bin = image_path.join("bin");
                if !image_bin.is_dir() {
                    continue;
                }
                let Ok(bin_entries) = fs::read_dir(&image_bin) else {
                    continue;
                };
                for bin_entry in bin_entries.flatten() {
                    if !bin_entry.path().is_file() {
                        continue;
                    }
                    let bin_name = bin_entry.file_name();
                    let target = format!(
                        "/var/images/{}/bin/{}",
                        image_name.to_string_lossy(),
                        bin_name.to_string_lossy()
                    );
                    let link = bin_dir.join(&bin_name);
                    let _ = fs::remove_file(&link);
                    std::os::unix::fs::symlink(&target, &link).with_context(|| {
                        format!(
                            "Failed to create /bin symlink {} -> {}",
                            link.display(),
                            target
                        )
                    })?;
                }
            }
        }
        println!("  Populated /bin with symlinks");
    }

    // Copy /etc/rc.boot + /etc/profile for shell-based boot
    let etc_dir = staging_dir.join("etc");
    fs::create_dir_all(&etc_dir)?;
    let system_toml_userdisk_src = project_root().join("etc/system.toml");
    if system_toml_userdisk_src.exists() {
        fs::copy(&system_toml_userdisk_src, etc_dir.join("system.toml"))?;
        println!("  Added /etc/system.toml");
    }
    let profile_src = project_root().join("etc/profile");
    if profile_src.exists() {
        fs::copy(&profile_src, etc_dir.join("profile"))?;
        println!("  Added /etc/profile");
    }
    let users_src = project_root().join("etc/users.toml");
    if users_src.exists() {
        fs::copy(&users_src, etc_dir.join("users.toml"))?;
        println!("  Added /etc/users.toml");
    }
    let envelopes_src = project_root().join("etc/envelopes.toml");
    if envelopes_src.exists() {
        fs::copy(&envelopes_src, etc_dir.join("envelopes.toml"))?;
        println!("  Added /etc/envelopes.toml");
    }
    let drivermgr_src = project_root().join("etc/drivermgr.toml");
    if drivermgr_src.exists() {
        fs::copy(&drivermgr_src, etc_dir.join("drivermgr.toml"))?;
        println!("  Added /etc/drivermgr.toml");
    }
    // UE20: ship system-wide and root's personal shellrc into the
    // userdisk so /bin/shell can source them on startup.
    let shellrc_src = project_root().join("etc/shellrc");
    if shellrc_src.exists() {
        fs::copy(&shellrc_src, etc_dir.join("shellrc"))?;
        println!("  Added /etc/shellrc");
    }
    let root_shellrc_src = project_root().join("home/root/.shellrc");
    if root_shellrc_src.exists() {
        fs::copy(&root_shellrc_src, home_root_dir.join(".shellrc"))?;
        println!("  Added /home/root/.shellrc");
    }

    // Visitor-friendly seeds: motd shown by login, plus welcome and
    // architecture files for `cat /etc/welcome.txt` exploration.
    for name in &["motd", "welcome.txt", "architecture.txt"] {
        let src = project_root().join("etc").join(name);
        if src.exists() {
            fs::copy(&src, etc_dir.join(name))?;
            println!("  Added /etc/{}", name);
        }
    }

    let gc_stress_src = project_root().join("userspace/micropython/gc_stress.py");
    if gc_stress_src.exists() {
        fs::copy(&gc_stress_src, etc_dir.join("gc_stress.py"))?;
        println!("  Added /etc/gc_stress.py");
    }

    for script in &[
        "color_256.py",
        "attr_render.py",
        "alt_screen.py",
        "mp_spike.py",
    ] {
        let src = project_root().join("userspace/micropython").join(script);
        if src.exists() {
            fs::copy(&src, etc_dir.join(script))?;
            println!("  Added /etc/{}", script);
        }
    }

    let edit_plugins_src = project_root().join("userspace/edit/plugins");
    if edit_plugins_src.is_dir() {
        let plugins_dir = etc_dir.join("edit/plugins");
        fs::create_dir_all(&plugins_dir)?;
        if let Ok(entries) = fs::read_dir(&edit_plugins_src) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "py") {
                    if let Some(name) = path.file_name() {
                        fs::copy(&path, plugins_dir.join(name))?;
                        println!("  Added /etc/edit/plugins/{}", name.to_string_lossy());
                    }
                }
            }
        }
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
            "1048576", // 1GB image (1048576 blocks * 1KiB)
        ])
        .status()
        .context("Failed to run mke2fs for user disk")?;

    if !status.success() {
        bail!("mke2fs failed while creating user disk image");
    }

    println!("  ✓ userdisk.img created");
    Ok(())
}

fn run_qemu(
    debug: bool,
    pin_core: Option<usize>,
    net: bool,
    port: u16,
    display: &str,
    virtio_gpu: bool,
) -> Result<()> {
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

    let qemu_bin = std::env::var("QEMU_BIN").unwrap_or_else(|_| {
        let local = format!(
            "{}/.local/bin/qemu-system-x86_64",
            std::env::var("HOME").unwrap_or_default()
        );
        if std::path::Path::new(&local).exists() {
            local
        } else {
            "qemu-system-x86_64".to_string()
        }
    });
    // Ensure custom QEMU finds its bundled libslirp (if built with internal slirp subproject).
    let local_lib = format!(
        "{}/.local/lib/x86_64-linux-gnu",
        std::env::var("HOME").unwrap_or_default()
    );
    if std::path::Path::new(&local_lib).exists() {
        let cur = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        let new_ld = if cur.is_empty() {
            local_lib.clone()
        } else {
            format!("{}:{}", local_lib, cur)
        };
        std::env::set_var("LD_LIBRARY_PATH", new_ld);
    }
    let mut cmd = if let Some(core) = pin_core {
        println!("▸ Pinning QEMU to host CPU core {}", core);
        let mut c = Command::new("taskset");
        c.args(["-c", &core.to_string(), &qemu_bin]);
        c
    } else {
        Command::new(&qemu_bin)
    };

    // KVM acceleration with host CPU for accurate instruction behavior
    // This exposes real hardware alignment requirements (SSE/AVX)
    cmd.args([
        "-bios",
        ovmf,
        "-m",
        "1G",
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
        "virtio-blk-pci,drive=userblk,disable-legacy=on,disable-modern=off",
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-pci,netdev=net0,disable-legacy=on,disable-modern=off",
        "-audiodev",
        "pa,id=snd0",
        "-device",
        "virtio-sound-pci,audiodev=snd0,addr=0x6,disable-legacy=on,disable-modern=off",
        // Host folder share via virtio-9p-pci (PCI slot 7 = addr=0x7).
        // slot 7 INTA# → PIRQD → IRQ 11 (shared with virtio-blk slot 3).
        // Kernel supports shared IRQ: both drivers receive IRQ 11, each
        // checks its own virtio ISR to claim/dismiss.
        // security_model=none maps uid/gid straight through (no xattr).
        "-fsdev",
        "local,id=hostshare,path=/home/vlb2bp/cluu-host-share,security_model=none",
        "-device",
        "virtio-9p-pci,fsdev=hostshare,mount_tag=hostshare,addr=0x7,disable-legacy=on,disable-modern=off",
        // USB 2.0 EHCI host controller + keyboard/mouse
        "-device",
        "usb-ehci,id=ehci",
        "-device",
        "usb-kbd,bus=ehci.0",
        "-device",
        "usb-mouse,bus=ehci.0",
        "-display",
        display,
        "-no-reboot",
        "-no-shutdown",
    ]);

    if virtio_gpu {
        cmd.args([
            "-vga",
            "none",
            "-device",
            "virtio-gpu-pci,max_outputs=1,edid=on",
        ]);
    }

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

    let mut httpd_child: Option<std::process::Child> = None;
    if net {
        let www_dir = std::env::temp_dir().join("cluu-httpd-demo");
        let _ = std::fs::create_dir_all(&www_dir);
        let _ = std::fs::write(
            www_dir.join("index.html"),
            "<html><body><h1>CLUU virtio-net demo</h1><p>Hello from the host!</p></body></html>",
        );
        match Command::new("python3")
            .args([
                "-m",
                "http.server",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
            ])
            .current_dir(&www_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                println!(
                    "  Network: host HTTP server on http://127.0.0.1:{} (pid {})",
                    port,
                    child.id()
                );
                println!("  From inside CLUU: curl 10.0.2.2:{}", port);
                httpd_child = Some(child);
            }
            Err(e) => {
                eprintln!("  Network: failed to start host HTTP server: {}", e);
            }
        }
    }

    let status = cmd.status().context("Failed to run QEMU")?;

    if let Some(mut child) = httpd_child {
        let _ = child.kill();
        let _ = child.wait();
    }

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
            "cluu-root-procmgr",
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
            "cluu-grep",
            "--exclude",
            "cluu-head",
            "--exclude",
            "cluu-tail",
            "--exclude",
            "cluu-wc",
            "--exclude",
            "cluu-ls",
            "--exclude",
            "cluu-ps",
            "--exclude",
            "cluu-touch",
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

// T21 BLOCKED — fceux requires C++ stdlib.
const BLOCKED_CONTAINERS: &[&str] = &["fceux"];

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
                        if BLOCKED_CONTAINERS.contains(&name) {
                            continue;
                        }
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
    "devmgr",
    "root-procmgr",
    "vfs",
    "drivermgr",
    "drivermon",
    "virtio-blk",
    "virtio-net",
    "virtio-9p",
    "virtio-snd",
    "virtio-gpu",
    "netd",
    "tpmd",
    "usb-input",
    "kbd",
    "mouse",
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
        ("cpuburn", "userspace/c-programs/cpuburn.c"),
        ("futexprobe", "userspace/c-programs/futexprobe.c"),
        ("futexrace", "userspace/c-programs/futexrace.c"),
        ("setjmpprobe", "userspace/c-programs/setjmpprobe.c"),
        ("envprobe", "userspace/c-programs/envprobe.c"),
        ("cfmismatch", "userspace/c-programs/cfmismatch.c"),
        ("stubsprobe", "userspace/c-programs/stubsprobe.c"),
        ("pipeprobe", "userspace/c-programs/pipeprobe.c"),
        ("pipecat", "userspace/c-programs/pipecat.c"),
        ("spawnpipeprobe", "userspace/c-programs/spawnpipeprobe.c"),
        ("tlsprobe", "userspace/c-programs/tlsprobe.c"),
        ("pthreadprobe", "userspace/c-programs/pthreadprobe.c"),
        ("errnoprobe", "userspace/c-programs/errnoprobe.c"),
        ("stackprobe", "userspace/c-programs/stackprobe.c"),
        ("dtachprobe", "userspace/c-programs/dtachprobe.c"),
        ("devprobe", "userspace/c-programs/devprobe.c"),
        ("fbprobe", "userspace/c-programs/fbprobe.c"),
        ("devfb0_probe", "userspace/c-programs/devfb0_probe.c"),
        ("containerprobe", "userspace/c-programs/containerprobe.c"),
        ("pwdprobe", "userspace/c-programs/pwdprobe.c"),
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
        // MicroPython's qstr generation has a known race with -j4 on clean
        // builds: gc.c references MP_QSTR___del__ before the qstrdefs header
        // is generated. A second make picks up the now-generated qstrdefs.
        let status2 = Command::new("make")
            .current_dir(&micropython_dir)
            .arg("-j4")
            .status()
            .context("Failed to build MicroPython (second pass)")?;
        if !status2.success() {
            bail!("MicroPython build failed");
        }
    }
    println!("  ✓ MicroPython built");
    Ok(())
}

fn build_doom() -> Result<()> {
    let doom_dir = project_root().join("userspace/doom-cluu");
    let doom_src = project_root().join("external/doomgeneric/doomgeneric/doomgeneric.h");
    if !doom_src.exists() {
        eprintln!("  Warning: doomgeneric source not found at {}, skipping DOOM build", doom_src.display());
        return Ok(());
    }

    println!("▸ Building doom-cluu Rust staticlib...");
    let target_spec = project_root().join("triplets/x86_64-cluu-user.json");
    let status = Command::new("cargo")
        .current_dir(project_root())
        .args(["build", "-p", "doom-cluu", "--target"])
        .arg(&target_spec)
        .arg("-Z")
        .arg("build-std=core,alloc,compiler_builtins")
        .status()
        .context("Failed to build doom-cluu staticlib")?;
    if !status.success() {
        bail!("doom-cluu Rust build failed");
    }

    let cluu_lib = project_root().join("target/sysroot/lib");
    let staticlib_src = project_root()
        .join("target/x86_64-cluu-user/debug/libdoom_cluu.a");
    let staticlib_dst = cluu_lib.join("libdoom_cluu.a");
    fs::create_dir_all(&cluu_lib).ok();
    fs::copy(&staticlib_src, &staticlib_dst).context("Failed to copy libdoom_cluu.a to sysroot")?;
    println!("  ✓ doom-cluu staticlib built and staged");

    // SDL2 is built separately by build_sdl2() and staged to
    // target/sysroot/lib/libSDL2.a.  The DOOM Makefile links against it
    // directly — no sdl2-cluu shim staticlib needed (shim retired in T19).

    println!("▸ Building DOOM...");
    let status = Command::new("make")
        .current_dir(&doom_dir)
        .arg("-j4")
        .status()
        .context("Failed to build DOOM")?;
    if !status.success() {
        bail!("DOOM build failed");
    }

    println!("  ✓ DOOM built");
    Ok(())
}

/// Build pinned SDL2 2.30.0 static library for the CLUU userspace target.
/// Vendored under userspace/sdl2/SDL2-2.30.0/. Produces libSDL2.a in the
/// sysroot. See .omo/evidence/task-14-cluu-multimedia-stack.txt.
fn build_sdl2() -> Result<()> {
    let sdl2_dir = project_root().join("userspace/sdl2");
    let sdl2_src = sdl2_dir.join("SDL2-2.30.0/src/SDL.c");
    if !sdl2_src.exists() {
        eprintln!(
            "  Warning: SDL2 source not found at {}, skipping SDL2 build",
            sdl2_src.display()
        );
        return Ok(());
    }

    let newlib_lib = project_root().join("target/sysroot/x86_64-cluu-elf/lib");
    if !newlib_lib.exists() {
        println!("▸ Building newlib (prerequisite for SDL2)...");
        ensure_newlib_installed()?;
    }

    let syscalls = project_root().join("target/sysroot/lib/libcluu_syscalls.a");
    if !syscalls.exists() {
        println!("▸ Building libcluu_syscalls (prerequisite for SDL2)...");
        build_syscalls("dev")?;
    }

    println!("▸ Building pinned SDL2 2.30.0 static library...");
    let status = Command::new("make")
        .current_dir(&sdl2_dir)
        .arg("-j4")
        .status()
        .context("Failed to build SDL2")?;
    if !status.success() {
        bail!("SDL2 build failed");
    }

    println!("  ✓ SDL2 static library built and staged to sysroot");
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

    let opt_flag = if profile == "dev" { "-O0" } else { "-O2" };
    // Section flags + linker --gc-sections shrink C bins ~10x by dropping
    // unreferenced newlib code (e.g. printf scaffolding pulled in for any
    // <stdio.h> include). Always on — same flags work in dev and release.
    let section_flags = ["-ffunction-sections", "-fdata-sections"];

    let compile_success = {
        // Try clang first
        let mut compile_cmd = Command::new("clang");
        compile_cmd.args([
            &format!("--target={}", CLUU_CLANG_TARGET),
            "-ffreestanding",
            "-fno-stack-protector",
            "-nostdlib",
            opt_flag,
        ]);
        compile_cmd.args(section_flags);
        compile_cmd.arg("-c");

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
                    opt_flag,
                ]);
                gcc_cmd.args(section_flags);
                gcc_cmd.arg("-c");

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

    // Strip debug + symbol info for non-dev builds: newlib is built with
    // `-g`, so the `.debug_*` sections survive even after --gc-sections
    // for everything that ends up referenced. Stripping recovers the rest
    // of the win (4.5 MB → ~50 KB for hello-world C bins).
    let strip_flag = if profile == "dev" {
        None
    } else {
        Some("--strip-all")
    };

    let link_success = {
        let mut link_cmd = Command::new("ld.lld");
        link_cmd.args(["-T", linker_script.to_str().unwrap(), "--gc-sections"]);
        if let Some(s) = strip_flag {
            link_cmd.arg(s);
        }
        link_cmd.args([
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
                ld_cmd.args(["-T", linker_script.to_str().unwrap(), "--gc-sections"]);
                if let Some(s) = strip_flag {
                    ld_cmd.arg(s);
                }
                ld_cmd.args([
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

    let mut built_count = 0;
    for cluufile in &cluufiles {
        let name = cluufile
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        if BLOCKED_CONTAINERS.contains(&name.as_str()) {
            println!("  Skipping blocked container: {}", name);
            continue;
        }
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
        built_count += 1;
    }

    println!("  ✓ {} container image(s) built", built_count);
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

fn check_cap_purity() -> Result<()> {
    let forbidden = [
        "pid_to_session",
        "tid_to_pid",
        "resolve_caller_session",
        "caller_profile",
        "can_grant",
        "session_match",
    ];
    let crates = ["userspace/root-procmgr", "userspace/session-procmgr"];
    // Legacy monolith pending ACL redesign — see project_procmgr_acl_redesign.
    // Violations here warn instead of fail until the monolith is retired.
    let legacy_warn_only = ["userspace/root-procmgr/src/main.rs"];
    let root = project_root();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for c in &crates {
        let crate_dir = root.join(c);
        if !crate_dir.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&crate_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |x| x == "rs"))
        {
            let body = std::fs::read_to_string(entry.path())
                .with_context(|| format!("Failed to read {}", entry.path().display()))?;
            let rel = entry
                .path()
                .strip_prefix(&root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            let is_legacy = legacy_warn_only.iter().any(|p| rel == *p);
            for kw in &forbidden {
                for (line, text) in body.lines().enumerate() {
                    if text.contains(kw) && !text.trim_start().starts_with("//") {
                        let msg = format!("{}:{}: {}", rel, line + 1, kw);
                        if is_legacy {
                            warnings.push(msg);
                        } else {
                            errors.push(msg);
                        }
                    }
                }
            }
        }
    }
    if !warnings.is_empty() {
        eprintln!(
            "cap-purity: {} legacy warning(s) in root-procmgr/src/main.rs (deferred to ACL redesign):",
            warnings.len()
        );
        for w in &warnings {
            eprintln!("  ⚠ {}", w);
        }
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("cap-purity violation: {}", e);
        }
        bail!(
            "{} cap-purity violation(s) in non-legacy files",
            errors.len()
        );
    }
    println!("  ✓ cap-purity: no violations in modular crates");
    Ok(())
}

/// Phase 14.1: Run cargo-llvm-cov on procmgr crates, enforce a line+branch
/// coverage threshold. The gated crate set matches the cap-refactor scope.
fn coverage_check(report_only: bool, threshold: f64) -> Result<()> {
    let probe = std::process::Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .output();
    let probe_ok = match probe {
        Ok(o) => o.status.success(),
        Err(_) => false,
    };
    if !probe_ok {
        eprintln!("cargo-llvm-cov not installed.");
        eprintln!("Install with: cargo install cargo-llvm-cov --locked");
        eprintln!("Then: rustup component add llvm-tools-preview");
        bail!("missing prerequisite: cargo-llvm-cov");
    }

    let packages = [
        "procmgr-common",
        "cluu-root-procmgr",
        "cluu-session-procmgr",
    ];
    let mut args: Vec<String> = vec![
        "llvm-cov".into(),
        "--features".into(),
        "host-test".into(),
        "--json".into(),
        "--summary-only".into(),
    ];
    for p in &packages {
        args.push("-p".into());
        args.push((*p).into());
    }

    println!("Running: cargo {}", args.join(" "));
    let out = std::process::Command::new("cargo")
        .args(&args)
        .current_dir(project_root())
        .output()
        .context("Failed to spawn cargo llvm-cov")?;
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        bail!("cargo llvm-cov exited non-zero");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Pull the totals block. cargo-llvm-cov JSON has data[0].totals
    // with `lines.percent` and `branches.percent` (some toolchains emit
    // `regions.percent` instead of branches; check both).
    let v: serde_json::Value =
        serde_json::from_str(&stdout).context("Failed to parse cargo llvm-cov JSON")?;
    let totals = v
        .get("data")
        .and_then(|d| d.get(0))
        .and_then(|x| x.get("totals"))
        .ok_or_else(|| anyhow::anyhow!("missing data[0].totals in coverage JSON"))?;

    let lines = totals
        .get("lines")
        .and_then(|l| l.get("percent"))
        .and_then(|p| p.as_f64());
    let branches = totals
        .get("branches")
        .and_then(|b| b.get("percent"))
        .and_then(|p| p.as_f64())
        .or_else(|| {
            totals
                .get("regions")
                .and_then(|r| r.get("percent"))
                .and_then(|p| p.as_f64())
        });

    let line_pct = lines.unwrap_or(0.0);
    let branch_pct = branches.unwrap_or(0.0);

    println!("Coverage (procmgr crates):");
    println!("  lines:    {:.2}% (threshold {:.2}%)", line_pct, threshold);
    println!(
        "  branches: {:.2}% (threshold {:.2}%)",
        branch_pct, threshold
    );

    let mut shortfalls = Vec::new();
    if line_pct < threshold {
        shortfalls.push(format!("lines {:.2}% < {:.2}%", line_pct, threshold));
    }
    if branch_pct < threshold {
        shortfalls.push(format!("branches {:.2}% < {:.2}%", branch_pct, threshold));
    }

    if shortfalls.is_empty() {
        println!("  ✓ coverage-check: thresholds met");
        return Ok(());
    }

    if report_only {
        eprintln!("  ⚠ coverage-check shortfalls (report-only):");
        for s in &shortfalls {
            eprintln!("    - {}", s);
        }
        return Ok(());
    }

    for s in &shortfalls {
        eprintln!("coverage-check: {}", s);
    }
    bail!(
        "coverage below threshold ({} shortfall(s))",
        shortfalls.len()
    );
}
