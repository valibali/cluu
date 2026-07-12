//! /bin/ping — ICMP echo utility via netd.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::args::args;
use libcluu::ipc::{
    call, call_with_payload, call_with_reply_buf, NET_BIND, NET_RECV, NET_SEND, NET_SOCKET,
    NET_SOCK_ICMP,
};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu};

const EAGAIN: isize = -11;
const PING_DATA: &[u8] = b"cluu-ping!";
const PING_IDENT: u16 = 0x4311;

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for i in 0..4 {
        out[i] = parts[i].parse().ok()?;
    }
    Some(out)
}

fn ipv4_word(o: &[u8; 4]) -> usize {
    ((o[0] as usize) << 24) | ((o[1] as usize) << 16) | ((o[2] as usize) << 8) | o[3] as usize
}

fn icmp_checksum(buf: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < buf.len() {
        sum += ((buf[i] as u32) << 8) | (buf[i + 1] as u32);
        i += 2;
    }
    if i < buf.len() {
        sum += (buf[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    (!sum & 0xffff) as u16
}

fn build_echo_request(ident: u16, seq: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(8 + PING_DATA.len());
    pkt.push(8);
    pkt.push(0);
    pkt.extend_from_slice(&[0, 0]);
    pkt.extend_from_slice(&ident.to_be_bytes());
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(PING_DATA);
    let cksum = icmp_checksum(&pkt);
    pkt[2] = (cksum >> 8) as u8;
    pkt[3] = (cksum & 0xff) as u8;
    pkt
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = registry::init("ping");

    let argv = args();
    if argv.len() < 2 {
        let _ = debug_print("ping: usage: ping <ipv4-addr>");
        return 2;
    }
    let dest = match parse_ipv4(&argv[1]) {
        Some(a) => a,
        None => {
            let _ = debug_print(&format!("ping: bad address '{}'", argv[1]));
            return 2;
        }
    };

    let netd_ep = match registry::subscribe_output("netd", "main") {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("ping: netd unavailable");
            return 1;
        }
    };

    let mut sock_msg = Message::new(NET_SOCKET, [NET_SOCK_ICMP, 0, 0, 0, 0, 0], 1);
    if call(netd_ep, &mut sock_msg, IpcFlags::empty()).is_err() {
        let _ = debug_print("ping: netd socket call failed");
        return 1;
    }
    let fd = sock_msg.words[0] as isize;
    if fd < 0 {
        let _ = debug_print("ping: could not create ICMP socket");
        return 1;
    }

    let mut bind_msg =
        Message::new(NET_BIND, [fd as usize, 0, PING_IDENT as usize, 0, 0, 0], 1);
    if call(netd_ep, &mut bind_msg, IpcFlags::empty()).is_err() {
        let _ = debug_print("ping: netd bind call failed");
        return 1;
    }
    if bind_msg.words[0] as isize != 0 {
        let _ = debug_print("ping: could not bind ICMP socket");
        return 1;
    }

    let pkt = build_echo_request(PING_IDENT, 1);
    let send_msg = Message::new(
        NET_SEND,
        [pkt.len(), fd as usize, ipv4_word(&dest), 0, 0, 0],
        1,
    );
    let mut send_reply = Message::new(0, [0; 6], 0);
    if call_with_payload(netd_ep, &send_msg, &pkt, &mut send_reply).is_err() {
        let _ = debug_print("ping: netd send failed");
        return 1;
    }
    if (send_reply.words[0] as isize) < 0 {
        let _ = debug_print(&format!("ping: send error {}", send_reply.words[0] as isize));
        return 1;
    }
    let _ = debug_print(&format!(
        "ping: echo request sent to {}.{}.{}.{}",
        dest[0], dest[1], dest[2], dest[3]
    ));

    let mut reply_buf = [0u8; 2120];
    let mut got_ok = false;
    for _ in 0..1000 {
        let recv_msg = Message::new(NET_RECV, [fd as usize, 2048, 0xFFFF_FFFF, 0, 0, 0], 1);
        match call_with_reply_buf(netd_ep, &recv_msg, &[], &mut reply_buf) {
            Ok((reply, plen)) => {
                let n = reply.words[0] as isize;
                if n == EAGAIN || n < 0 {
                    let _ = yield_cpu();
                    continue;
                }
                if plen >= 8 {
                    let header = core::mem::size_of::<Message>();
                    let payload = &reply_buf[header..header + plen];
                    if payload[0] == 0
                        && payload[4] == (PING_IDENT >> 8) as u8
                        && payload[5] == (PING_IDENT & 0xff) as u8
                        && payload[6] == 0
                        && payload[7] == 1
                    {
                        got_ok = true;
                        break;
                    }
                }
                let _ = yield_cpu();
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }

    if got_ok {
        let _ = debug_print(&format!(
            "ping: {}.{}.{}.{} reply ok",
            dest[0], dest[1], dest[2], dest[3]
        ));
        let _ = debug_print("PING_OK");
        0
    } else {
        let _ = debug_print("ping: no reply (timeout)");
        1
    }
}
