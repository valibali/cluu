//! Shared default VFS view policy by capability profile.

use crate::cap::CapProfile;
use alloc::string::String;
use alloc::vec::Vec;

pub type ViewMountSpec = (&'static str, &'static str, bool);

const SUPERVISOR_MOUNTS: &[ViewMountSpec] = &[("/", "/", true)];
const DEVICE_MOUNTS: &[ViewMountSpec] = &[
    ("/bin", "/bin", false),
    ("/lib", "/lib", false),
    ("/dev", "/dev", true),
    ("/etc", "/etc", false),
    ("/tmp", "/tmp", true),
    ("/dev/initrd", "/dev/initrd", false),
    ("/proc", "/proc", false),
];
const ADMIN_MOUNTS: &[ViewMountSpec] = &[
    ("/bin", "/bin", false),
    ("/lib", "/lib", false),
    ("/etc", "/etc", false),
    ("/tmp", "/tmp", true),
    ("/home/root", "/home/root", true),
    ("/host", "/host", true),
    ("/dev/initrd", "/dev/initrd", false),
    ("/dev/null", "/dev/null", false),
    ("/dev/zero", "/dev/zero", false),
    ("/dev/urandom", "/dev/urandom", false),
    ("/dev/tty", "/dev/tty", false),
    ("/dev/pts", "/dev/pts", true),
    ("/dev/console", "/dev/console", false),
    ("/proc", "/proc", false),
];
const USER_MOUNTS: &[ViewMountSpec] = &[
    ("/bin", "/bin", false),
    ("/lib", "/lib", false),
    ("/etc", "/etc", false),
    ("/tmp", "/tmp", true),
    ("/home/root", "/home/root", true),
    ("/host", "/host", true),
    ("/dev/initrd", "/dev/initrd", false),
    ("/dev/null", "/dev/null", false),
    ("/dev/zero", "/dev/zero", false),
    ("/dev/urandom", "/dev/urandom", false),
    ("/dev/tty", "/dev/tty", false),
    ("/dev/pts", "/dev/pts", true),
    ("/proc", "/proc", false),
];
const EMPTY_MOUNTS: &[ViewMountSpec] = &[];

pub fn default_mounts_for_profile(profile: CapProfile) -> &'static [ViewMountSpec] {
    if profile.contains(CapProfile::ADMIN) {
        SUPERVISOR_MOUNTS
    } else if profile.contains(CapProfile::DEVICE) {
        DEVICE_MOUNTS
    } else if profile.contains(CapProfile::VFS) {
        USER_MOUNTS
    } else {
        EMPTY_MOUNTS
    }
}

/// Return the default mounts for an ADMIN-profile login session.
/// This gives USER mounts plus read-only /etc access.
pub fn admin_session_mounts() -> &'static [ViewMountSpec] {
    ADMIN_MOUNTS
}

/// Return default mounts for `profile` with `home` substituted in place of the
/// hardcoded `/home/root` entry.
///
/// For profiles whose default mount set already grants full root (`/` → `/`),
/// the base set is returned unchanged — full root already covers the user's
/// home, and appending it would create a redundant duplicate mount.
pub fn default_mounts_for_profile_and_home(
    profile: CapProfile,
    home: &str,
) -> Vec<(String, String, bool)> {
    let base_mounts: &[ViewMountSpec] = if profile == CapProfile::ADMIN_PROFILE {
        admin_session_mounts()
    } else {
        default_mounts_for_profile(profile)
    };
    // Full-root mount set: return as-is.
    if base_mounts.iter().any(|&(src, _, _)| src == "/") {
        return base_mounts
            .iter()
            .map(|&(src, dst, w)| (String::from(src), String::from(dst), w))
            .collect();
    }
    let mut mounts: Vec<(String, String, bool)> = base_mounts
        .iter()
        .filter(|&&(_, dst, _)| !dst.starts_with("/home/"))
        .map(|&(src, dst, w)| (String::from(src), String::from(dst), w))
        .collect();
    mounts.push((String::from(home), String::from(home), true));
    mounts
}
