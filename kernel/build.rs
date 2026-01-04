use std::path::PathBuf;

fn main() {
    let _out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Link pre-assembled NASM objects
    // (assembled by xtask before cargo build)
    let asm_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .join("target/asm");

    if asm_dir.exists() {
        println!("cargo:rustc-link-search=native={}", asm_dir.display());

        for entry in std::fs::read_dir(&asm_dir).unwrap() {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "o") {
                    let name = path.file_stem().unwrap().to_str().unwrap();
                    // Create archive for linking
                    let ar_path = asm_dir.join(format!("lib{}.a", name));
                    let _ = std::process::Command::new("ar")
                        .args(["rcs", ar_path.to_str().unwrap(), path.to_str().unwrap()])
                        .status();
                    println!("cargo:rustc-link-lib=static={}", name);
                }
            }
        }
    }

    // Link font.o for framebuffer
    let kernel_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let font_o = kernel_dir.join("font.o");

    if font_o.exists() {
        println!("cargo:rustc-link-search=native={}", kernel_dir.display());
        let font_a = kernel_dir.join("libfont.a");
        let _ = std::process::Command::new("ar")
            .args(["rcs", font_a.to_str().unwrap(), font_o.to_str().unwrap()])
            .status();
        println!("cargo:rustc-link-lib=static=font");
    } else {
        // If font.o doesn't exist, create it from font.psf
        let font_psf = kernel_dir.join("font.psf");
        if font_psf.exists() {
            let status = std::process::Command::new("objcopy")
                .args([
                    "-O", "elf64-x86-64",
                    "-B", "i386",
                    "-I", "binary",
                    font_psf.to_str().unwrap(),
                    font_o.to_str().unwrap(),
                ])
                .status();

            if status.is_ok() && status.unwrap().success() {
                println!("cargo:rustc-link-search=native={}", kernel_dir.display());
                let font_a = kernel_dir.join("libfont.a");
                let _ = std::process::Command::new("ar")
                    .args(["rcs", font_a.to_str().unwrap(), font_o.to_str().unwrap()])
                    .status();
                println!("cargo:rustc-link-lib=static=font");
            }
        }
    }

    // Rerun if assembly sources change
    println!("cargo:rerun-if-changed=src/arch/x86_64/boot.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/context.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/interrupts.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/syscall_entry.asm");

    // Rerun if font changes
    println!("cargo:rerun-if-changed=font.psf");
    println!("cargo:rerun-if-changed=font.o");
}
