//! Build orchestration for CLUU
//!
//! Usage:
//!   cargo xtask build          # Build everything
//!   cargo xtask run            # Build and run in QEMU
//!   cargo xtask test           # Run all tests
//!   cargo xtask clean          # Clean all build artifacts

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    },
    /// Build and run in QEMU
    Run {
        #[arg(long, default_value = "dev")]
        profile: String,
        /// Enable debug mode (pause for GDB, telnet serial on 4321)
        #[arg(long)]
        debug: bool,
    },
    /// Run all tests
    Test,
    /// Clean all build artifacts
    Clean,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { profile } => {
            build_userspace(&profile)?;
            build_kernel(&profile)?;
            create_initrd(&profile)?;
            create_disk_image(&profile)?;
            println!("✓ Build complete: target/cluu.img");
        }
        Commands::Run { profile, debug } => {
            build_userspace(&profile)?;
            build_kernel(&profile)?;
            create_initrd(&profile)?;
            create_disk_image(&profile)?;
            run_qemu(debug)?;
        }
        Commands::Test => {
            run_tests()?;
        }
        Commands::Clean => {
            clean()?;
        }
        Commands::Userspace { profile } => {
            build_userspace(&profile)?;
        }
        Commands::Kernel { profile } => {
            build_kernel(&profile)?;
        }
    }

    Ok(())
}

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn build_userspace(profile: &str) -> Result<()> {
    println!("▸ Building userspace programs...");

    let userspace_crates = [
        "userspace/libcluu", // Library must be first
        "userspace/hello",   // Examples
        "userspace/cap_demo",
        "userspace/init", // System programs
        "userspace/procmgr",
        "userspace/vfs",
        "userspace/ramfs",
        "userspace/console",
        "userspace/shell",
        "userspace/cat",
    ];

    let target_json = project_root().join("triplets/x86_64-cluu-user.json");

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

    // First, assemble NASM files if they exist
    let _ = assemble_nasm();

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

fn assemble_nasm() -> Result<()> {
    println!("  Assembling NASM files...");

    let asm_dir = project_root().join("kernel/src/architecture/x86_64");
    let out_dir = project_root().join("target/asm");

    fs::create_dir_all(&out_dir)?;

    let asm_files = ["boot.asm", "context.asm", "interrupts.asm", "syscall_entry.asm"];

    for asm_file in &asm_files {
        let src = asm_dir.join(asm_file);
        if !src.exists() {
            continue; // Skip if file doesn't exist yet
        }

        let obj_name = asm_file.replace(".asm", ".o");
        let obj = out_dir.join(&obj_name);

        let status = Command::new("nasm")
            .args([
                "-f",
                "elf64",
                "-g",
                "-F",
                "dwarf",
                "-o",
                obj.to_str().unwrap(),
                src.to_str().unwrap(),
            ])
            .status()
            .context("Failed to run NASM")?;

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

    // Create directory structure
    fs::create_dir_all(initrd_dir.join("sys"))?;
    fs::create_dir_all(initrd_dir.join("bin"))?;
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

    // Copy system servers to initrd/sys/
    let sys_programs = ["init", "procmgr", "vfs", "ramfs", "console"];
    for prog in &sys_programs {
        let src = userspace_target_dir.join(format!("{}.elf", prog));
        let dst = initrd_dir.join("sys").join(prog);
        if src.exists() {
            fs::copy(&src, &dst).with_context(|| format!("Failed to copy {}", prog))?;
            println!("  Copied sys/{}", prog);
        } else {
            println!("  Warning: {} not found, skipping", prog);
        }
    }

    // Copy user programs to initrd/bin/
    let bin_programs = ["shell", "cat"];
    for prog in &bin_programs {
        let src = userspace_target_dir.join(format!("{}.elf", prog));
        let dst = initrd_dir.join("bin").join(prog);
        if src.exists() {
            fs::copy(&src, &dst).with_context(|| format!("Failed to copy {}", prog))?;
            println!("  Copied bin/{}", prog);
        } else {
            println!("  Warning: {} not found, skipping", prog);
        }
    }

    // Create etc/motd
    fs::write(initrd_dir.join("etc/motd"), "Welcome to CLUU!\n")?;

    println!("  ✓ initrd directory created");
    Ok(())
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

    // Create bootboot config file
    let bootboot_config = "// BOOTBOOT configuration\nscreen=1024x768\nkernel=sys/core\n";
    fs::write(
        project_root().join("target/bootboot_config"),
        bootboot_config,
    )?;

    let output_img = project_root().join("target/cluu.img");

    // Check if mkbootimg exists
    let mkbootimg_path = project_root().join("utilies/mkbootimg/mkbootimg");
    if !mkbootimg_path.exists() {
        println!("  Building mkbootimg...");
        let status = Command::new("make")
            .current_dir(project_root().join("utilies/mkbootimg"))
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

fn run_qemu(debug: bool) -> Result<()> {
    let img_path = project_root().join("target/cluu.img");

    if !img_path.exists() {
        bail!("Disk image not found. Run 'cargo xtask build' first.");
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
    cmd.args([
        "-bios",
        ovmf,
        "-m",
        "256M",
        "-drive",
        &format!("file={},format=raw", img_path.display()),
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
        println!("");
        println!("  Quick start:");
        println!("    Terminal 1: cargo xtask run --debug");
        println!("    Terminal 2: telnet localhost 4321");
        println!("    Terminal 3: gdb target/x86_64-cluu-kernel/debug/deps/kernel-*.elf");
        println!("                (gdb) target remote :1234");
        println!("                (gdb) continue");
        println!("");

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
    let _ = fs::remove_dir_all(project_root().join("target/asm"));

    println!("  ✓ Cleaned");
    Ok(())
}
