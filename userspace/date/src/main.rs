//! /bin/date — print or format the current date and time.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::posix::_write;

fn spec() -> Spec {
    Spec::new()
        .program("date")
        .version("0.1.0")
        .usage("[+FORMAT]")
        .flag('u', "utc", "use UTC (no timezone adjustment — CLUU has no TZ)")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn current_unix_time() -> u64 {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    extern "C" {
        fn clock_gettime(clock_id: i32, tp: *mut Timespec) -> i32;
    }
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        clock_gettime(0 /* CLOCK_REALTIME */, &mut ts);
        if ts.tv_sec > 0 {
            ts.tv_sec as u64
        } else {
            0
        }
    }
}

/// Civil calendar from days since 1970-01-01.
/// Returns (year, month[1-12], day[1-31], hour[0-23], min[0-59], sec[0-59]).
fn unix_to_components(t: u64) -> (u32, u32, u32, u32, u32, u32) {
    let secs_in_day = 86_400u64;
    let days = t / secs_in_day;
    let rem = t % secs_in_day;
    let hh = (rem / 3600) as u32;
    let mm = ((rem % 3600) / 60) as u32;
    let ss = (rem % 60) as u32;

    // Proleptic Gregorian — Hinnant's algorithm.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y_off = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp as u32 + 3 } else { mp as u32 - 9 };
    let y = (if m <= 2 { y_off + 1 } else { y_off }) as u32;
    (y, m, d, hh, mm, ss)
}

const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const DAYS_SHORT: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

fn day_of_week(t: u64) -> &'static str {
    // 1970-01-01 was a Thursday (index 0).
    let days = (t / 86_400) as usize;
    DAYS_SHORT[days % 7]
}

/// Format a unix timestamp using a strftime-like format string.
/// Supported: %Y %y %m %d %H %M %S %e %b %a %n %t %%
fn format_date(t: u64, fmt: &str) -> String {
    let (y, m, d, hh, mm, ss) = unix_to_components(t);
    let dow = day_of_week(t);
    let mon_name = MONTHS_SHORT[(m as usize).saturating_sub(1).min(11)];

    let mut out = String::new();
    let bytes = fmt.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            i += 1;
            if i >= bytes.len() {
                out.push('%');
                break;
            }
            match bytes[i] {
                b'Y' => out.push_str(&format!("{:04}", y)),
                b'y' => out.push_str(&format!("{:02}", y % 100)),
                b'm' => out.push_str(&format!("{:02}", m)),
                b'd' => out.push_str(&format!("{:02}", d)),
                b'e' => out.push_str(&format!("{:2}", d)),
                b'H' => out.push_str(&format!("{:02}", hh)),
                b'M' => out.push_str(&format!("{:02}", mm)),
                b'S' => out.push_str(&format!("{:02}", ss)),
                b'b' | b'h' => out.push_str(mon_name),
                b'a' => out.push_str(dow),
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'%' => out.push('%'),
                other => {
                    out.push('%');
                    out.push(other as char);
                }
            }
            i += 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
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
            write_fd(1, b"date 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("date: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let t = current_unix_time();

    let output = if let Some(fmt_arg) = parsed.positional.first() {
        if let Some(fmt) = fmt_arg.strip_prefix('+') {
            format_date(t, fmt)
        } else {
            let msg = format!("date: invalid argument '{}'\n", fmt_arg);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    } else {
        // Default: "Day Mon DD HH:MM:SS YYYY"
        let (y, m, d, hh, mm, ss) = unix_to_components(t);
        let dow = day_of_week(t);
        let mon = MONTHS_SHORT[(m as usize).saturating_sub(1).min(11)];
        format!("{} {} {:02} {:02}:{:02}:{:02} {:04}", dow, mon, d, hh, mm, ss, y)
    };

    write_fd(1, output.as_bytes());
    write_fd(1, b"\n");
    let _ = debug_print("date: ok (exit 0)");
    0
}
