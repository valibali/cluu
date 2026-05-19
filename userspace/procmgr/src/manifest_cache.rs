//! Per-image manifest cache.
//!
//! Holds the parsed Cluufile state keyed by image name. Lazily populated
//! on first miss via the existing manifest-reading helper.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use spin::Mutex;

use cluu_proto::spawn::RestartPolicy;

/// Cached projection of a Cluufile manifest.
#[derive(Clone, Debug)]
pub struct CachedManifest {
    /// Full path to the entrypoint binary, e.g., "/bin/shell".
    pub entrypoint: String,
    /// Restart policy declared by the Cluufile (defaults to Never if absent).
    pub restart_policy: RestartPolicy,
    /// Whether the manifest grants `RIGHT_SESSIONLESS_SPAWN`.
    pub allow_sessionless: bool,
}

pub struct ManifestCache {
    inner: Mutex<BTreeMap<String, CachedManifest>>,
}

impl ManifestCache {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(BTreeMap::new()) }
    }

    /// Look up by image name. On miss, calls `loader` (which must read the
    /// manifest from VFS and build a `CachedManifest`). Returns `None` if
    /// the loader fails (image not found, parse error).
    pub fn get_or_load<F>(&self, image: &str, loader: F) -> Option<CachedManifest>
    where
        F: FnOnce() -> Option<CachedManifest>,
    {
        {
            let guard: &mut BTreeMap<String, CachedManifest> = &mut *self.inner.lock();
            if let Some(m) = guard.get(image) {
                return Some(m.clone());
            }
        }
        let loaded = loader()?;
        let mut guard = self.inner.lock();
        guard.entry(image.into()).or_insert(loaded.clone());
        Some(loaded)
    }

    /// Invalidate one entry (used when an image is reinstalled).
    pub fn invalidate(&self, image: &str) {
        self.inner.lock().remove(image);
    }
}

/// Singleton instance. Procmgr's main module holds the loader closure.
pub static MANIFEST_CACHE: ManifestCache = ManifestCache::new();