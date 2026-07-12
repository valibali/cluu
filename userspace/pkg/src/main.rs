//! pkg — local package manager.
//!
//! Commands: pkg list, pkg install <name>, pkg remove <name>.
//! List: scan /var/images/*/manifest.toml.
//! Install: print what would be installed (copy binary stub).
//! Remove: print what would be removed.
//! No network — local packages only.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

const IMAGES_DIR: &str = "/var/images";
const BIN_DIR: &str = "/bin";

fn vfs() -> Option<VfsClient> {
    let ep = registry::subscribe_output("vfs", "main").ok()?;
    let cid = registry::control_endpoint();
    Some(VfsClient::new(ep, cid))
}

fn write_out(s: &str) {
    let _ = _write(1, s.as_ptr() as *const _, s.len());
}

fn list_packages() -> Vec<String> {
    let v = match vfs() {
        Some(v) => v,
        None => return Vec::new(),
    };
    match v.readdir(IMAGES_DIR) {
        Ok(entries) => entries
            .into_iter()
            .filter(|e| e.is_dir)
            .map(|e| e.name)
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn cmd_list() -> i32 {
    let packages = list_packages();
    if packages.is_empty() {
        write_out("No packages found in /var/images\n");
        return 0;
    }
    write_out(&format!("Found {} package(s):\n", packages.len()));
    for name in &packages {
        let manifest = format!("{}/{}/manifest.toml", IMAGES_DIR, name);
        let line = format!("  {} ({})\n", name, manifest);
        write_out(&line);
    }
    0
}

fn cmd_install(name: &str) -> i32 {
    let packages = list_packages();
    if !packages.iter().any(|p| p == name) {
        write_out(&format!("pkg: package '{}' not found in {}\n", name, IMAGES_DIR));
        return 1;
    }
    let dest = format!("{}/{}", BIN_DIR, name);
    let line = format!(
        "pkg: would install '{}' -> {}\n",
        name, dest
    );
    write_out(&line);
    write_out("pkg: (binary copy not yet implemented — stub)\n");
    0
}

fn cmd_remove(name: &str) -> i32 {
    let dest = format!("{}/{}", BIN_DIR, name);
    let line = format!(
        "pkg: would remove '{}' from {}\n",
        name, dest
    );
    write_out(&line);
    write_out("pkg: (removal not yet implemented — stub)\n");
    0
}

fn print_usage() {
    write_out(
        "Usage: pkg <command> [args]\n\
         Commands:\n\
         \x20 list            List installed packages\n\
         \x20 install <name>  Install a local package\n\
         \x20 remove <name>   Remove a package\n",
    );
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("PKG_OK\n");
    let args = libcluu::args::args();

    if args.len() < 2 {
        print_usage();
        return 0;
    }

    match args[1].as_str() {
        "list" => cmd_list(),
        "install" => {
            if args.len() < 3 {
                write_out("pkg: install requires a package name\n");
                return 1;
            }
            cmd_install(&args[2])
        }
        "remove" => {
            if args.len() < 3 {
                write_out("pkg: remove requires a package name\n");
                return 1;
            }
            cmd_remove(&args[2])
        }
        "--help" | "-h" => {
            print_usage();
            0
        }
        other => {
            write_out(&format!("pkg: unknown command '{}'\n", other));
            print_usage();
            2
        }
    }
}
