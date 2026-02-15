//! Environment variable support for newlib compatibility.
//!
//! Provides getenv/setenv/unsetenv/environ with static storage.
//! Environment is initialized from ProcessInfo.params[8..9] at __cluu_init() time.

use super::{c_char, c_int};
use core::ptr;
use spin::Mutex;

/// Maximum number of environment variables.
const MAX_ENV_VARS: usize = 64;
/// Maximum total bytes for all env string storage (KEY=VALUE\0 packed).
const ENV_STORAGE_BYTES: usize = 2048;

struct EnvStorage {
    /// Packed "KEY=VALUE\0" strings.
    buf: [u8; ENV_STORAGE_BYTES],
    /// Next free byte in buf.
    buf_used: usize,
    /// Pointers into buf for each "KEY=VALUE" string.
    /// Each entry points to the start of a string within buf.
    entries: [usize; MAX_ENV_VARS], // offsets into buf
    /// Lengths of each entry (not including NUL terminator).
    lengths: [usize; MAX_ENV_VARS],
    /// Number of active entries.
    count: usize,
    /// The C `environ` pointer array (pointers into buf, NULL-terminated).
    /// Must be rebuilt after any mutation.
    ptrs: [*mut c_char; MAX_ENV_VARS + 1],
    /// Whether the storage has been initialized.
    initialized: bool,
}

unsafe impl Send for EnvStorage {}
unsafe impl Sync for EnvStorage {}

impl EnvStorage {
    const fn new() -> Self {
        Self {
            buf: [0; ENV_STORAGE_BYTES],
            buf_used: 0,
            entries: [0; MAX_ENV_VARS],
            lengths: [0; MAX_ENV_VARS],
            count: 0,
            ptrs: [ptr::null_mut(); MAX_ENV_VARS + 1],
            initialized: false,
        }
    }

    /// Find the index of an entry by key name.
    fn find(&self, key: &[u8]) -> Option<usize> {
        for i in 0..self.count {
            let offset = self.entries[i];
            let len = self.lengths[i];
            let entry = &self.buf[offset..offset + len];
            // Check if entry starts with "key="
            if entry.len() > key.len()
                && entry[key.len()] == b'='
                && entry[..key.len()] == *key
            {
                return Some(i);
            }
        }
        None
    }

    /// Get the value for a key. Returns pointer to the value part (after '=').
    fn get(&self, key: &[u8]) -> *mut c_char {
        if let Some(idx) = self.find(key) {
            let offset = self.entries[idx];
            // Skip past "KEY="
            let value_offset = offset + key.len() + 1;
            return &self.buf[value_offset] as *const u8 as *mut c_char;
        }
        ptr::null_mut()
    }

    /// Set a key=value pair. If overwrite is 0 and key exists, do nothing.
    /// Returns 0 on success, -1 on error.
    fn set(&mut self, key: &[u8], value: &[u8], overwrite: bool) -> c_int {
        // "KEY=VALUE\0" needs key.len() + 1 + value.len() + 1 bytes
        let needed = key.len() + 1 + value.len() + 1;

        if let Some(idx) = self.find(key) {
            if !overwrite {
                return 0;
            }
            // Remove old entry and add new one
            self.remove_index(idx);
        }

        if self.count >= MAX_ENV_VARS {
            return -1;
        }
        if self.buf_used + needed > ENV_STORAGE_BYTES {
            return -1;
        }

        let offset = self.buf_used;
        self.buf[offset..offset + key.len()].copy_from_slice(key);
        self.buf[offset + key.len()] = b'=';
        self.buf[offset + key.len() + 1..offset + key.len() + 1 + value.len()]
            .copy_from_slice(value);
        self.buf[offset + needed - 1] = 0; // NUL terminator

        self.entries[self.count] = offset;
        self.lengths[self.count] = needed - 1; // length without NUL
        self.count += 1;
        self.buf_used += needed;

        self.rebuild_ptrs();
        0
    }

    /// Remove an entry by index, compacting the entries array.
    fn remove_index(&mut self, idx: usize) {
        if idx >= self.count {
            return;
        }
        // Shift entries down (don't bother compacting buf - fragmentation is acceptable
        // for the small number of env vars we support)
        for i in idx..self.count - 1 {
            self.entries[i] = self.entries[i + 1];
            self.lengths[i] = self.lengths[i + 1];
        }
        self.count -= 1;
        self.rebuild_ptrs();
    }

    /// Remove an entry by key name. Returns 0 on success, -1 if not found.
    fn unset(&mut self, key: &[u8]) -> c_int {
        if let Some(idx) = self.find(key) {
            self.remove_index(idx);
            0
        } else {
            // POSIX says unsetenv for nonexistent var is not an error
            0
        }
    }

    /// Rebuild the `ptrs` array and update the global `environ`.
    fn rebuild_ptrs(&mut self) {
        for i in 0..self.count {
            self.ptrs[i] = &self.buf[self.entries[i]] as *const u8 as *mut c_char;
        }
        self.ptrs[self.count] = ptr::null_mut();

        // Update global environ pointer
        unsafe {
            environ = self.ptrs.as_mut_ptr();
        }
    }

    /// Add a "KEY=VALUE" entry from raw packed data (already formatted).
    fn add_raw(&mut self, kv: &[u8]) {
        if self.count >= MAX_ENV_VARS {
            return;
        }
        let needed = kv.len() + 1; // +1 for NUL
        if self.buf_used + needed > ENV_STORAGE_BYTES {
            return;
        }

        let offset = self.buf_used;
        self.buf[offset..offset + kv.len()].copy_from_slice(kv);
        self.buf[offset + kv.len()] = 0;

        self.entries[self.count] = offset;
        self.lengths[self.count] = kv.len();
        self.count += 1;
        self.buf_used += needed;
    }
}

static ENV: Mutex<EnvStorage> = Mutex::new(EnvStorage::new());

/// Global environment pointer required by newlib.
///
/// Updated whenever the environment is mutated.
#[no_mangle]
pub static mut environ: *mut *mut c_char = core::ptr::null_mut();

/// Initialize environment from ProcessInfo page.
///
/// Reads params[8] (envc) and params[9] (env_offset) from ProcessInfo.
/// The env data is packed "KEY=VALUE\0KEY=VALUE\0..." at the given page offset.
pub fn init_env() {
    let info = crate::boot::process_info();
    let envc = info.params[crate::boot::PARAM_ENVC] as usize;
    let env_offset = info.params[crate::boot::PARAM_ENV_OFFSET] as usize;

    let mut env = ENV.lock();
    if env.initialized {
        return;
    }
    env.initialized = true;

    if envc == 0 || env_offset == 0 {
        env.rebuild_ptrs();
        return;
    }

    // Read env data from the ProcessInfo page
    let page_base = crate::boot::PROCESS_INFO_ADDR & !(4096 - 1);
    let data_ptr = (page_base + env_offset) as *const u8;

    // Parse packed "KEY=VALUE\0" strings
    let mut ptr = data_ptr;
    let page_end = page_base + 4096;
    let mut parsed = 0usize;

    while parsed < envc && (ptr as usize) < page_end {
        // Find NUL terminator
        let start = ptr;
        let mut len = 0usize;
        unsafe {
            while (start.add(len) as usize) < page_end && *start.add(len) != 0 {
                len += 1;
            }
        }
        if len == 0 {
            break;
        }

        let kv = unsafe { core::slice::from_raw_parts(start, len) };
        env.add_raw(kv);
        parsed += 1;

        // Skip past the NUL terminator
        ptr = unsafe { start.add(len + 1) };
    }

    env.rebuild_ptrs();
}

/// Convert a C string pointer to a byte slice (without NUL).
unsafe fn cstr_bytes(ptr: *const c_char) -> &'static [u8] {
    if ptr.is_null() {
        return &[];
    }
    let mut len = 0;
    let mut p = ptr;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }
    core::slice::from_raw_parts(ptr as *const u8, len)
}

/// Get an environment variable by name.
#[no_mangle]
pub extern "C" fn getenv(name: *const c_char) -> *mut c_char {
    if name.is_null() {
        return ptr::null_mut();
    }
    let key = unsafe { cstr_bytes(name) };
    if key.is_empty() {
        return ptr::null_mut();
    }
    let env = ENV.lock();
    env.get(key)
}

/// Set an environment variable.
#[no_mangle]
pub extern "C" fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int {
    if name.is_null() {
        return -1;
    }
    let key = unsafe { cstr_bytes(name) };
    if key.is_empty() || key.contains(&b'=') {
        return -1;
    }
    let val = if value.is_null() {
        &[] as &[u8]
    } else {
        unsafe { cstr_bytes(value) }
    };
    let mut env = ENV.lock();
    env.set(key, val, overwrite != 0)
}

/// Remove an environment variable.
#[no_mangle]
pub extern "C" fn unsetenv(name: *const c_char) -> c_int {
    if name.is_null() {
        return -1;
    }
    let key = unsafe { cstr_bytes(name) };
    if key.is_empty() || key.contains(&b'=') {
        return -1;
    }
    let mut env = ENV.lock();
    env.unset(key)
}
