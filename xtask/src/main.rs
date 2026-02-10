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

const CLUU_TARGET_TRIPLET: &str = "x86_64-cluu";
const NEWLIB_TARGET_TRIPLET: &str = "x86_64-unknown-elf";
const NEWLIB_CLUU_TRIPLET: &str = "x86_64-cluu-elf";
const CLUU_CLANG_TARGET: &str = "x86_64-unknown-none-elf";

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { profile } => {
            build_userspace(&profile)?;
            build_kernel(&profile)?;
            build_c_programs(&profile)?;
            create_initrd(&profile)?;
            create_user_block_image(&profile)?;
            create_disk_image(&profile)?;
            println!("✓ Build complete: target/cluu.img");
        }
        Commands::Run { profile, debug } => {
            build_userspace(&profile)?;
            build_kernel(&profile)?;
            build_c_programs(&profile)?;
            create_initrd(&profile)?;
            create_user_block_image(&profile)?;
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
        "userspace/shell",
        "userspace/timeserver",
        "userspace/cat",
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

fn assemble_nasm() -> Result<()> {
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
    let sys_programs = [
        "init",
        "procmgr",
        "registry",
        "timeserver",
        "vfs",
        "console",
        "kbd",
        "tty",
        "virtio-blk",
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

    // Copy user programs to initrd/bin/
    let bin_programs = ["shell"];
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

    // Note: C programs are intentionally NOT in the initrd.
    // They are placed on the ext2 disk and spawned via VFS.

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
    let mut out = String::from("# CLUU boot manifest\nmanifest_version=1\n");
    for path in service_paths {
        let data = fs::read(initrd_dir.join(path))
            .with_context(|| format!("Failed to read service image '{}'", path))?;
        let digest = legacy_hash_sha256(&data);
        let digest_hex = to_lower_hex(&digest);
        let rights_mask = manifest_rights_mask(path);
        out.push_str(&format!(
            "service path={} sha256={} rights=0x{:08x}\n",
            path, digest_hex, rights_mask
        ));
    }
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
        _ => ALL_RIGHTS_MASK,
    }
}

// Must stay in sync with klibcluu::crypto::hash_sha256 for now.
fn legacy_hash_sha256(data: &[u8]) -> [u8; 32] {
    let mut hash = [0u8; 32];
    for (i, chunk) in data.chunks(32).enumerate() {
        for (j, &byte) in chunk.iter().enumerate() {
            hash[j] ^= byte.wrapping_add((i as u8).wrapping_mul(j as u8));
        }
    }
    for i in 0..hash.len() {
        hash[i] = hash[i]
            .wrapping_add(hash[(i + 7) % hash.len()])
            .wrapping_mul(17);
    }
    hash
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

fn create_user_block_image(profile: &str) -> Result<()> {
    println!("▸ Creating virtio-blk userspace image...");

    let cargo_profile = if profile == "dev" { "debug" } else { profile };

    let userspace_target_dir = project_root()
        .join("target/x86_64-cluu-user")
        .join(cargo_profile);

    let staging_dir = project_root().join("userfs");
    let bin_dir = staging_dir.join("bin");
    let _ = fs::remove_dir_all(&bin_dir);
    fs::create_dir_all(&bin_dir)?;

    let bin_programs = ["shell"];
    for prog in &bin_programs {
        let src = userspace_target_dir.join(format!("{}.elf", prog));
        let dst = bin_dir.join(prog);
        if !src.exists() {
            bail!("{} not found in {:?}", prog, userspace_target_dir);
        }
        fs::copy(&src, &dst).with_context(|| format!("Failed to copy {}", prog))?;
        println!("  Added {}", prog);
    }

    // Also add any C programs (built via cargo xtask build-c)
    let c_programs = ["hello"];
    for prog in &c_programs {
        let src = userspace_target_dir.join(format!("{}.elf", prog));
        let dst = bin_dir.join(prog);
        if src.exists() {
            fs::copy(&src, &dst).with_context(|| format!("Failed to copy {}", prog))?;
            println!("  Added {} (C program)", prog);
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
            "32768", // 32MB image (32768 blocks * 1KiB)
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
        bail!("Disk image not found. Run 'cargo xtask build' first.");
    }
    if !user_disk.exists() {
        bail!("User disk image not found. Run 'cargo xtask build' first.");
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
    //let _ = fs::remove_dir_all(project_root().join("target/userfs"));
    let _ = fs::remove_file(project_root().join("target/userdisk.img"));
    let _ = fs::remove_dir_all(project_root().join("target/asm"));

    println!("  ✓ Cleaned");
    Ok(())
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

fn build_newlib() -> Result<()> {
    println!("▸ Building newlib...");

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

/// Build all C programs in userspace/c_hello etc.
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
        let newlib_src = project_root().join("external").join(format!("newlib-{}", "4.4.0.20231231"));
        if newlib_src.exists() {
            println!("▸ Installing newlib to sysroot (required for C programs)...");
            ensure_newlib_installed()?;
        } else {
            println!("  ⚠ Newlib not found - C programs will have limited libc support");
            println!("    To enable full C library: ./scripts/download-newlib.sh && cargo xtask build-newlib");
        }
    }

    // List of C programs to build: (name, source_path)
    let c_programs: &[(&str, &str)] = &[("hello", "userspace/c_hello/minimal.c")];

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
    let out_dir = project_root()
        .join("target/x86_64-cluu-user")
        .join(cargo_profile);
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

    // Step 3: Check for newlib
    let newlib_src = project_root().join("external/newlib-4.4.0.20231231");
    if newlib_src.exists() {
        println!("");
        println!("  Newlib source found. Building...");
        build_newlib()?;
    } else {
        println!("");
        println!("  Newlib source not found.");
        println!("  To enable full C library support, run:");
        println!("    ./scripts/download-newlib.sh");
        println!("    cargo xtask build-newlib");
    }

    println!("");
    println!("✓ C toolchain setup complete!");
    println!("");
    println!("Sysroot: {}", sysroot_path().display());
    println!("");
    println!("To build a C program:");
    println!("  cargo xtask build-c <name> <source.c>");
    println!("");
    println!("Example:");
    println!("  cargo xtask build-c hello userspace/c_hello/hello.c");

    Ok(())
}
