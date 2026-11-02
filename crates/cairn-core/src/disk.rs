//! How much room is actually there.
//!
//! An extraction writes 40–45 GB. Discovering that the disk is full 20 GB in leaves
//! a half-written tree and a user who has to work out what happened, so the check
//! happens before the first file: `run()` compares the plan's total against the free
//! space of the destination's filesystem and refuses up front when it cannot fit.
//!
//! `statvfs` on the destination, not on `/`: the game folder is frequently on a
//! different drive, which is exactly the case where a wrong answer is expensive.

use std::path::Path;

/// Bytes available to the current user at `path`, or `None` when it cannot be
/// asked (the folder does not exist yet, or the call is unsupported).
pub fn free_bytes(path: &Path) -> Option<u64> {
    // statvfs needs a path that exists. Walk up to the nearest ancestor so a
    // destination that has not been created yet still gets a real answer — the
    // filesystem it will be created on is the same one.
    let mut probe = path;
    loop {
        if probe.exists() {
            break;
        }
        probe = match probe.parent() {
            Some(parent) if parent != probe => parent,
            _ => return None,
        };
    }

    let c_path = std::ffi::CString::new(probe.as_os_str().to_string_lossy().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }

    // `f_bavail` is blocks available to unprivileged users — the number `df` shows,
    // and the one that matters because the extraction does not run as root.
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_directory_reports_a_plausible_amount() {
        let free = free_bytes(&std::env::temp_dir()).expect("temp dir must be measurable");
        assert!(free > 1024 * 1024, "suspiciously small: {free}");
    }

    #[test]
    fn a_destination_that_does_not_exist_yet_walks_up_to_its_parent() {
        let missing = std::env::temp_dir().join("cairn-disk-nonexistent/deeper/still");
        let free = free_bytes(&missing).expect("the parent filesystem is measurable");
        assert!(free > 1024 * 1024);
    }

    #[test]
    fn free_space_of_a_file_is_the_filesystem_it_sits_on() {
        // A path that exists as a file rather than a directory: statvfs still
        // answers for its filesystem rather than failing.
        let file = std::env::temp_dir().join("cairn-disk-probe.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(free_bytes(&file).is_some());
        std::fs::remove_file(&file).ok();
    }
}
