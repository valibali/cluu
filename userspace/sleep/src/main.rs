//! /bin/sleep — delay for specified seconds.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::String;
use alloc::vec::Vec;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::posix::_write;

fn spec() -> Spec {
    Spec::new()
        .program("sleep")
        .version("0.1.0")
        .usage("SECONDS")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn do_nanosleep(secs: u64) {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    extern "C" {
        fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32;
    }
    let req = Timespec {
        tv_sec: secs as i64,
        tv_nsec: 0,
    };
    unsafe {
        nanosleep(&req, core::ptr::null_mut());
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let sp = spec();
    let parsed = match parse(&sp, &argv) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            write_fd(1, render_help(&sp).as_bytes());
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"sleep 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = alloc::format!("sleep: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"sleep: missing operand\n");
        return 2;
    }

    let secs: u64 = match parsed.positional[0].parse() {
        Ok(n) => n,
        Err(_) => {
            let msg = alloc::format!(
                "sleep: invalid time interval '{}'\n",
                parsed.positional[0]
            );
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    do_nanosleep(secs);
    let _ = debug_print("sleep: ok (exit 0)");
    0
}
