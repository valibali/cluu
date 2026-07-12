//! /bin/curl — minimal HTTP client (curl-like).
//!
//! Usage: curl [-o FILE] [-s] [-I] URL
//! -o FILE: write output to file instead of stdout
//! -s: silent (suppress progress)
//! -I: HEAD request (headers only)

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::ffi::c_void;
use libcluu::debug_print;
use libcluu::posix::socket;

fn parse_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.find(':') {
        Some(i) => {
            let p: u16 = host_port[i + 1..].parse().ok()?;
            (host_port[..i].to_string(), p)
        }
        None => (host_port.to_string(), 80),
    };
    Some((host, port, path.to_string()))
}

fn parse_ip_literal(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut ip: u32 = 0;
    for p in parts {
        let octet: u32 = p.parse().ok()?;
        if octet > 255 {
            return None;
        }
        ip = (ip << 8) | octet;
    }
    Some(ip)
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    if args.len() < 2 {
        let _ = debug_print("CURL_FAIL: usage: curl [-o FILE] [-s] [-I] URL\n");
        return 1;
    }

    let mut output_file: Option<&str> = None;
    let mut silent = false;
    let mut head_only = false;
    let mut url: Option<&str> = None;

    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "-o" && i + 1 < args.len() {
            output_file = Some(args[i + 1].as_str());
            i += 2;
        } else if arg == "-s" {
            silent = true;
            i += 1;
        } else if arg == "-I" {
            head_only = true;
            i += 1;
        } else if !arg.starts_with('-') {
            url = Some(arg);
            i += 1;
        } else {
            i += 1;
        }
    }

    let url = match url {
        Some(u) => u,
        None => {
            let _ = debug_print("CURL_FAIL: no URL\n");
            return 1;
        }
    };

    let (host, port, path) = match parse_url(url) {
        Some(v) => v,
        None => {
            let _ = debug_print("CURL_FAIL: invalid URL\n");
            return 1;
        }
    };

    let ip = match parse_ip_literal(&host) {
        Some(ip) => ip,
        None => match socket::net_dns_resolve(&host) {
            Some(ip) => ip,
            None => {
                let _ = debug_print(&format!("CURL_FAIL: DNS resolve failed for {}\n", host));
                return 1;
            }
        },
    };

    let fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if fd < 0 {
        let _ = debug_print("CURL_FAIL: socket\n");
        return 1;
    }

    if socket::connect(fd, ip, port) != 0 {
        let _ = debug_print(&format!(
            "CURL_FAIL: connect to {}.{}.{}.{}:{}\n",
            (ip >> 24) & 0xff,
            (ip >> 16) & 0xff,
            (ip >> 8) & 0xff,
            ip & 0xff,
            port
        ));
        let _ = socket::close_socket(fd);
        return 1;
    }

    let method = if head_only { "HEAD" } else { "GET" };
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: cluu-curl/0.1\r\nAccept: */*\r\n\r\n",
        method, path, host
    );
    let req_bytes = request.as_bytes();
    let sent = socket::send(fd, req_bytes.as_ptr(), req_bytes.len(), 0);
    if sent != req_bytes.len() as isize {
        let _ = debug_print("CURL_FAIL: send\n");
        let _ = socket::close_socket(fd);
        return 1;
    }

    let mut total: usize = 0;
    let mut buf = [0u8; 4096];
    loop {
        let n = socket::recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n <= 0 {
            break;
        }
        let n = n as usize;
        if let Some(_filename) = output_file {
            let _ = libcluu::posix::_write(1, buf.as_ptr() as *const c_void, n);
        } else {
            let _ = libcluu::posix::_write(1, buf.as_ptr() as *const c_void, n);
        }
        total += n;
    }

    let _ = socket::close_socket(fd);

    if total == 0 {
        let _ = debug_print("CURL_FAIL: no data received\n");
        return 1;
    }

    if !silent {
        let _ = debug_print(&format!("CURL_OK bytes={}\n", total));
    } else {
        let _ = debug_print("CURL_OK\n");
    }
    0
}
