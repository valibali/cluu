//! Shared default VFS view policy by capability profile.

use crate::cap::CapProfile;

pub type ViewMountSpec = (&'static str, &'static str, bool);

const SUPERVISOR_MOUNTS: &[ViewMountSpec] = &[("/", "/", true)];
const DEVICE_MOUNTS: &[ViewMountSpec] = &[
    ("/bin", "/bin", false),
    ("/lib", "/lib", false),
    ("/dev", "/dev", true),
    ("/etc", "/etc", false),
    ("/tmp", "/tmp", true),
    ("/dev/initrd", "/dev/initrd", false),
];
const ADMIN_MOUNTS: &[ViewMountSpec] = &[
    ("/bin", "/bin", false),
    ("/lib", "/lib", false),
    ("/etc", "/etc", false),
    ("/tmp", "/tmp", true),
    ("/home/root", "/home/root", true),
    ("/dev/initrd", "/dev/initrd", false),
];
const USER_MOUNTS: &[ViewMountSpec] = &[
    ("/bin", "/bin", false),
    ("/lib", "/lib", false),
    ("/tmp", "/tmp", true),
    ("/home/root", "/home/root", true),
    ("/dev/initrd", "/dev/initrd", false),
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
