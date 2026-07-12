//! Editor plugin IPC — line-delimited JSON over pipes to MicroPython children.

extern crate alloc;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use crate::mode::Editor;
use core::sync::atomic::{AtomicBool, Ordering};
use libcluu::debug_print;

static PLUGINS_LOADED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub enum Json { Null, Bool(bool), Num(f64), Str(String), Arr(Vec<Json>), Obj(Vec<(String, Json)>) }

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        if let Json::Obj(p) = self { p.iter().find(|(k, _)| k == key).map(|(_, v)| v) } else { None }
    }
    fn as_str(&self) -> Option<&str> { if let Json::Str(s) = self { Some(s) } else { None } }
    fn as_num(&self) -> Option<usize> { if let Json::Num(n) = self { Some(*n as usize) } else { None } }
}

fn parse_json(s: &str) -> Option<Json> {
    let b = s.as_bytes(); let mut i = 0usize;
    fn sk(b: &[u8], i: &mut usize) { while *i < b.len() && matches!(b[*i], b' '|b'\t'|b'\r'|b'\n') { *i += 1; } }
    fn pv(b: &[u8], i: &mut usize) -> Option<Json> {
        sk(b, i); let c = *b.get(*i)?;
        match c {
            b'"' => ps(b, i).map(Json::Str), b'{' => po(b, i), b'[' => pa(b, i),
            b't' => { *i += 4; Some(Json::Bool(true)) }, b'f' => { *i += 5; Some(Json::Bool(false)) },
            b'n' => { *i += 4; Some(Json::Null) }, b'-' | b'0'..=b'9' => pn(b, i), _ => None,
        }
    }
    fn ps(b: &[u8], i: &mut usize) -> Option<String> {
        *i += 1; let mut s = String::new();
        while *i < b.len() { let c = b[*i]; *i += 1; if c == b'"' { return Some(s); }
            if c == b'\\' && *i < b.len() { let e = b[*i]; *i += 1; s.push(match e { b'"'=>'"', b'\\'=>'\\', b'n'=>'\n', b't'=>'\t', _=>'?' }); }
            else { s.push(c as char); } } None
    }
    fn pn(b: &[u8], i: &mut usize) -> Option<Json> {
        let s0 = *i; if *i < b.len() && b[*i] == b'-' { *i += 1; }
        while *i < b.len() && matches!(b[*i], b'0'..=b'9'|b'.'|b'e'|b'E'|b'+'|b'-') { *i += 1; }
        core::str::from_utf8(&b[s0..*i]).ok()?.parse::<f64>().ok().map(Json::Num)
    }
    fn pa(b: &[u8], i: &mut usize) -> Option<Json> {
        *i += 1; let mut v = Vec::new(); sk(b, i);
        if *i < b.len() && b[*i] == b']' { *i += 1; return Some(Json::Arr(v)); }
        loop { v.push(pv(b, i)?); sk(b, i); match b.get(*i)? { &b',' => *i += 1, &b']' => { *i += 1; break; }, _ => return None } }
        Some(Json::Arr(v))
    }
    fn po(b: &[u8], i: &mut usize) -> Option<Json> {
        *i += 1; let mut v = Vec::new(); sk(b, i);
        if *i < b.len() && b[*i] == b'}' { *i += 1; return Some(Json::Obj(v)); }
        loop { sk(b, i); let k = ps(b, i)?; sk(b, i); if b.get(*i) != Some(&b':') { return None; } *i += 1; v.push((k, pv(b, i)?)); sk(b, i); match b.get(*i)? { &b',' => *i += 1, &b'}' => { *i += 1; break; }, _ => return None } }
        Some(Json::Obj(v))
    }
    pv(b, &mut i)
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() { match c { '"' => o.push_str("\\\""), '\\' => o.push_str("\\\\"), '\n' => o.push_str("\\n"), '\t' => o.push_str("\\t"), c => o.push(c) } }
    o.push('"'); o
}

use cluu_wire::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope, ViewSource};
use libcluu::ipc::PROCMGR_PIPE_CREATE_LABEL;
use libcluu::posix::pipe::{read_pipe, write_pipe};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::IpcFlags;
use libcluu::boot::{TOKEN_EXTRA_0, TOKEN_SPACE, process_info};
use libcluu::syscall::ipc_recv_timeout;

const PIPE_DATA: u32 = 0x50495045;
const PIPE_EOF: u32 = 0x454F4644;

fn read_pipe_timeout(ep: usize, buf: &mut [u8], timeout_ms: u64) -> isize {
    let mut recv_buf = [0u8; 4 + 4092];
    match ipc_recv_timeout(ep, &mut recv_buf, timeout_ms) {
        Ok(len) if len >= 4 => {
            let label = u32::from_le_bytes([recv_buf[0], recv_buf[1], recv_buf[2], recv_buf[3]]);
            if label == PIPE_EOF { return 0; }
            if label != PIPE_DATA { return -1; }
            let data_len = len.saturating_sub(4);
            let to_copy = data_len.min(buf.len());
            buf[..to_copy].copy_from_slice(&recv_buf[4..4 + to_copy]);
            to_copy as isize
        }
        _ => -1,
    }
}

fn create_pipe() -> Option<(usize, usize)> {
    let ep = registry::lookup_service("procmgr:spawn")?;
    let mut req = Message::new(PROCMGR_PIPE_CREATE_LABEL, [0; 6], 0);
    libcluu::ipc::call(ep, &mut req, IpcFlags::empty()).ok()?;
    if req.words[0] != 0 { return None; }
    Some((req.words[1], req.words[2])) // (write_token, read_token)
}

struct PipeCh { w: usize, r: usize, buf: Vec<u8> }
impl PipeCh {
    fn send(&mut self, line: &str) { let mut b = line.as_bytes().to_vec(); b.push(b'\n'); let _ = write_pipe(self.w, &b); }
    fn recv_line(&mut self) -> Option<String> {
        loop {
            if let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=nl).collect();
                return Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
            }
            let mut tmp = [0u8; 4092];
            let n = read_pipe(self.r, &mut tmp);
            if n <= 0 { return None; }
            self.buf.extend_from_slice(&tmp[..n as usize]);
        }
    }
    fn recv_line_timeout(&mut self, timeout_ms: u64) -> Option<String> {
        loop {
            if let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=nl).collect();
                return Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
            }
            let mut tmp = [0u8; 4092];
            let n = read_pipe_timeout(self.r, &mut tmp, timeout_ms);
            if n <= 0 { return None; }
            self.buf.extend_from_slice(&tmp[..n as usize]);
        }
    }
}

struct Plugin { chan: PipeCh, keymap: Vec<(String, String)>, commands: Vec<(String, String)> }
pub struct PluginRegistry { plugins: Vec<Plugin> }

const PLUGIN_DIR: &str = "/etc/edit/plugins/";
const KNOWN: &[&str] = &["test_plugin.py", "syntax_highlight.py", "auto_indent.py", "status_mode.py"];

impl PluginRegistry {
    pub fn empty() -> Self { PluginRegistry { plugins: Vec::new() } }
    pub fn load_all() -> Self {
        if PLUGINS_LOADED.swap(true, Ordering::SeqCst) {
            let _ = debug_print("edit: plugins already loaded, skipping\n");
            return Self::empty();
        }
        let mut reg = Self::empty();
        for name in KNOWN { let _ = reg.try_load(&format!("{}{}", PLUGIN_DIR, name), name); }
        let _ = debug_print(&format!("edit: loaded {} plugin(s)\n", reg.plugins.len()));
        reg
    }

    fn try_load(&mut self, path: &str, name: &str) -> Result<(), String> {
        let script = read_vfs_file(path)?;
        let (sw, sr) = create_pipe().ok_or(String::from("pipe1"))?;
        let (ow, or_) = create_pipe().ok_or(String::from("pipe2"))?;
        let env = SpawnEnvelope {
            image: String::from("edit-plugin"),
            args: alloc::vec![String::from("micropython"), String::from("-c"), script],
            env: Vec::new(),
            view: ViewSource::Derive(libcluu::token(TOKEN_EXTRA_0) as u64),
            fd_inherit: alloc::vec![
                FdInherit { child_fd: 0, source: FdSource::EndpointCap { endpoint_token: sr as u64 }, rights: FdRights::READ_ONLY },
                FdInherit { child_fd: 1, source: FdSource::EndpointCap { endpoint_token: ow as u64 }, rights: FdRights::WRITE_ONLY },
            ],
            session: None, notify: None,
        };
        match libcluu::spawn::spawn(env) {
            Ok(r) => { let _ = debug_print(&format!("edit: plugin {} pid={}\n", name, r.pid)); }
            Err(e) => { let _ = debug_print(&format!("edit: plugin {} fail {:?}\n", name, e)); return Err(format!("{:?}", e)); }
        }
        let mut chan = PipeCh { w: sw, r: or_, buf: Vec::new() };
        let line = chan.recv_line_timeout(2000).ok_or("no reg (timeout)")?;
        let reg = parse_json(&line).ok_or("bad reg json")?;
        let mut km = Vec::new();
        let mut cmds = Vec::new();
        if let Some(j) = reg.get("keymap") { if let Json::Obj(p) = j { for (k, v) in p { if let Some(cb) = v.as_str() { km.push((k.clone(), String::from(cb))); } } } }
        if let Some(j) = reg.get("commands") { if let Json::Obj(p) = j { for (k, v) in p { if let Some(cb) = v.as_str() { cmds.push((k.clone(), String::from(cb))); } } } }
        let _ = debug_print(&format!("edit: plugin {} keys={} cmds={}\n", name, km.len(), cmds.len()));
        self.plugins.push(Plugin { chan, keymap: km, commands: cmds });
        Ok(())
    }

    pub fn has_key(&self, key: &str) -> Option<String> {
        for p in &self.plugins { for (k, c) in &p.keymap { if k == key { return Some(c.clone()); } } } None
    }
    pub fn has_command(&self, cmd: &str) -> Option<String> {
        for p in &self.plugins { for (c, cb) in &p.commands { if c == cmd { return Some(cb.clone()); } } } None
    }

    pub fn dispatch_key(&mut self, ed: &mut Editor, key: &str, cb: &str) -> bool {
        for i in 0..self.plugins.len() {
            if self.plugins[i].keymap.iter().any(|(k, c)| k == key && c == cb) {
                let ev = format!("{{\"type\":\"event\",\"event\":\"key\",\"key\":{},\"callback_id\":{}}}", json_str(key), json_str(cb));
                return self.dispatch(i, ed, &ev);
            }
        } false
    }
    pub fn dispatch_command(&mut self, ed: &mut Editor, cmd: &str, cb: &str) -> bool {
        for i in 0..self.plugins.len() {
            if self.plugins[i].commands.iter().any(|(c, b)| c == cmd && b == cb) {
                let ev = format!("{{\"type\":\"event\",\"event\":\"command\",\"command\":{},\"callback_id\":{}}}", json_str(cmd), json_str(cb));
                return self.dispatch(i, ed, &ev);
            }
        } false
    }

    fn dispatch(&mut self, idx: usize, ed: &mut Editor, ev: &str) -> bool {
        self.plugins[idx].chan.send(ev);
        loop {
            let line = match self.plugins[idx].chan.recv_line() { Some(l) => l, None => return false };
            let msg = match parse_json(&line) { Some(j) => j, None => return false };
            match msg.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "done" => { let _ = debug_print("PLUGIN_API_OK\n"); return true; }
                "call" => {
                    let m = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let p = msg.get("params").cloned().unwrap_or(Json::Null);
                    let id = msg.get("id").and_then(|v| v.as_num()).unwrap_or(0);
                    let res = exec_api(ed, m, &p);
                    self.plugins[idx].chan.send(&format!("{{\"type\":\"result\",\"id\":{},\"result\":{}}}", id, res));
                }
                _ => return false,
            }
        }
    }
}

fn exec_api(ed: &mut Editor, method: &str, p: &Json) -> String {
    match method {
        "buffer.read" => {
            let s = p.get("start").and_then(|v| v.as_num()).unwrap_or(0);
            let e = p.get("end").and_then(|v| v.as_num()).unwrap_or(0);
            let all = ed.buf.pieces.read_all();
            let lines: Vec<&[u8]> = all.split(|&b| b == b'\n').collect();
            let s = s.min(lines.len()); let e = e.min(lines.len()).max(s);
            let mut a = String::from("[");
            for (i, l) in lines[s..e].iter().enumerate() { if i > 0 { a.push(','); } a.push_str(&json_str(core::str::from_utf8(l).unwrap_or(""))); }
            a.push(']'); a
        }
        "buffer.insert" => {
            let pos = p.get("pos").and_then(|v| v.as_num()).unwrap_or(0);
            let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if ed.buf.pieces.insert(pos, text.as_bytes()).is_some() { ed.buf.mark_dirty(); }
            String::from("null")
        }
        "buffer.delete" => {
            let s = p.get("start").and_then(|v| v.as_num()).unwrap_or(0);
            let e = p.get("end").and_then(|v| v.as_num()).unwrap_or(0);
            if s < e { ed.buf.pieces.delete(s..e); ed.buf.mark_dirty(); }
            String::from("null")
        }
        "cursor.pos" => { let (r, c) = ed.buf.pieces.line_col(ed.buf.cursor); format!("[{},{}]", r, c) }
        "cursor.move" => {
            let row = p.get("row").and_then(|v| v.as_num()).unwrap_or(0);
            let col = p.get("col").and_then(|v| v.as_num()).unwrap_or(0);
            let idx = ed.buf.pieces.line_index();
            if row < idx.len() { ed.buf.cursor = idx[row] + col; }
            String::from("null")
        }
        "view.status" => { ed.message = String::from(p.get("text").and_then(|v| v.as_str()).unwrap_or("")); String::from("null") }
        "view.syntax" | "keymap.register" | "command.register" => String::from("null"),
        _ => String::from("null"),
    }
}

/// Read a file via VFS read_grant (zero-copy, mirrors vfs_io pattern).
fn read_vfs_file(path: &str) -> Result<String, String> {
    let ep = registry::subscribe_output("vfs", "main").map_err(|_| String::from("vfs sub"))?;
    let vfs = libcluu::fs::client::VfsClient::new_from_registry(ep).map_err(|_| String::from("vfs cli"))?;
    let file = vfs.open(path).map_err(|e| format!("open {:?}: {:?}", path, e))?;
    let total = file.size;
    if total > 65536 { let _ = vfs.close(file); return Err(format!("{} too large", path)); }
    if total == 0 { let _ = vfs.close(file); return Ok(String::new()); }
    let st = process_info().tokens[TOKEN_SPACE];
    if st == 0 { let _ = vfs.close(file); return Err(String::from("no space token")); }
    let alloc_sz = ((total.min(4096)) + 4095) & !4095;
    let scratch = match libcluu::vspace::VSPACE.lock().alloc(alloc_sz) { Ok(b) => b, Err(e) => { let _ = vfs.close(file); return Err(format!("vspace {:?}", e)); } };
    let mut out = Vec::with_capacity(total);
    let mut off = 0usize;
    let mut err: Option<String> = None;
    while off < total {
        let want = (total - off).min(4096);
        match vfs.read_grant(file, off, want, st, scratch) {
            Ok(g) => { if g.len == 0 { break; } unsafe { out.extend_from_slice(core::slice::from_raw_parts(scratch as *const u8, g.len)); } off += g.len; }
            Err(e) => { err = Some(format!("read {:?}", e)); break; }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(scratch, alloc_sz);
    let _ = vfs.close(file);
    err.map(Err).unwrap_or_else(|| String::from_utf8(out).map_err(|_| String::from("not utf8")))
}
