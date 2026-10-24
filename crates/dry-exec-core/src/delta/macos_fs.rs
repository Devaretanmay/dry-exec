//! macOS APFS Copy-on-Write (clonefile) filesystem snapshotting and diffing engine.

use crate::delta::types::FsMutation;
use crate::error::DeltaError;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

extern "C" {
    pub fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32)
        -> libc::c_int;
}

pub const CLONE_NOFOLLOW: u32 = 0x0001;
pub const CLONE_NOOWNERCOPY: u32 = 0x0002;
pub const CLONE_ACL: u32 = 0x0004;

/// Instantly snapshots a target directory into an ephemeral scratch path via APFS clonefile.
pub fn apfs_clone_directory(src: &Path, dst: &Path) -> Result<(), DeltaError> {
    crate::delta::clone::reject_overlap(src, dst)?;
    if !src.exists() {
        fs::create_dir_all(dst)?;
        return Ok(());
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }

    // Ensure dst does not already exist as clonefile requires non-existing destination
    if dst.exists() {
        let _ = fs::remove_dir_all(dst);
    }

    let c_src = CString::new(src.as_os_str().as_bytes())
        .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;
    let c_dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;

    let ret = unsafe { clonefile(c_src.as_ptr(), c_dst.as_ptr(), CLONE_NOFOLLOW) };
    if ret != 0 {
        // Fallback for directory trees across non-APFS volumes or sub-paths
        if dst.exists() {
            let _ = fs::remove_dir_all(dst);
        }
        fs::create_dir_all(dst)?;
        recursive_clone_or_copy(src, dst)?;
    }

    Ok(())
}

fn recursive_clone_or_copy(src: &Path, dst: &Path) -> Result<(), DeltaError> {
    let metadata = fs::symlink_metadata(src)?;
    if metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
    } else if metadata.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let dest_path = dst.join(entry.file_name());
            recursive_clone_or_copy(&path, &dest_path)?;
        }
        fs::set_permissions(dst, metadata.permissions())?;
    } else if metadata.is_file() {
        let c_src = CString::new(src.as_os_str().as_bytes())
            .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;
        let c_dst = CString::new(dst.as_os_str().as_bytes())
            .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;
        if unsafe { clonefile(c_src.as_ptr(), c_dst.as_ptr(), CLONE_NOFOLLOW) } != 0 {
            fs::copy(src, dst)?;
        }
    } else {
        return Err(DeltaError::ClonefileError(format!(
            "Unsupported workspace inode: {}",
            src.display()
        )));
    }
    Ok(())
}

/// Computes the filesystem state delta between the baseline state and ephemeral mutations.
pub fn compute_macos_fs_delta(
    baseline: &Path,
    ephemeral: &Path,
) -> Result<Vec<FsMutation>, DeltaError> {
    crate::delta::fs::FsSnapshot::capture(baseline)?.compute_delta_at(ephemeral)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn direct_apfs_clone_rejects_source_as_destination() {
        let source = tempdir().unwrap();
        fs::write(source.path().join("keep"), b"keep").unwrap();
        assert!(apfs_clone_directory(source.path(), source.path()).is_err());
        assert_eq!(fs::read(source.path().join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn fallback_clone_preserves_directory_and_dangling_symlinks() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("private"), b"private").unwrap();
        std::os::unix::fs::symlink(outside.path(), src.path().join("directory_link")).unwrap();
        std::os::unix::fs::symlink("missing", src.path().join("dangling_link")).unwrap();
        recursive_clone_or_copy(src.path(), dst.path()).unwrap();
        for name in ["directory_link", "dangling_link"] {
            assert!(fs::symlink_metadata(dst.path().join(name))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                fs::read_link(src.path().join(name)).unwrap(),
                fs::read_link(dst.path().join(name)).unwrap()
            );
        }
    }

    #[test]
    fn delta_preserves_symlinks_and_empty_directories_without_following_links() {
        let baseline = tempdir().unwrap();
        let current = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("private.txt"), b"private").unwrap();
        std::os::unix::fs::symlink(outside.path(), current.path().join("link")).unwrap();
        std::os::unix::fs::symlink("missing", current.path().join("broken")).unwrap();
        fs::create_dir(current.path().join("empty")).unwrap();
        let delta = compute_macos_fs_delta(baseline.path(), current.path()).unwrap();
        let paths: Vec<_> = delta
            .iter()
            .map(|mutation| mutation.path().to_path_buf())
            .collect();
        assert_eq!(
            paths,
            vec!["broken", "empty", "link"]
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn delta_reports_deleted_root_contents() {
        let baseline = tempdir().unwrap();
        let current = tempdir().unwrap();
        fs::write(baseline.path().join("removed"), b"data").unwrap();
        let missing = current.path().join("missing");
        let delta = compute_macos_fs_delta(baseline.path(), &missing).unwrap();
        assert!(matches!(&delta[..], [FsMutation::Deleted { .. }]));
    }

    #[test]
    fn test_apfs_clone_and_delta_computation() {
        let src_dir = tempdir().expect("tempdir");
        let dst_dir = tempdir().expect("tempdir");

        let test_file = src_dir.path().join("data.txt");
        fs::write(&test_file, b"initial baseline").expect("write");

        let clone_target = dst_dir.path().join("cloned_sandbox");
        apfs_clone_directory(src_dir.path(), &clone_target).expect("apfs clone");

        assert!(clone_target.join("data.txt").exists());

        // Mutate existing file and create new file in clone
        fs::write(clone_target.join("data.txt"), b"mutated data").expect("write mutation");
        fs::write(clone_target.join("new_file.txt"), b"brand new").expect("write new");

        let deltas = compute_macos_fs_delta(src_dir.path(), &clone_target).expect("compute delta");
        assert_eq!(deltas.len(), 2);

        let created_found = deltas
            .iter()
            .any(|d| matches!(d, FsMutation::Created { .. }));
        let modified_found = deltas
            .iter()
            .any(|d| matches!(d, FsMutation::Modified { .. }));
        assert!(created_found);
        assert!(modified_found);
    }
}
