//! macOS APFS Copy-on-Write (clonefile) filesystem snapshotting and diffing engine.

use crate::delta::types::{ByteDelta, FsMutation};
use crate::error::DeltaError;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::Read;
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

    let c_src = CString::new(src.to_str().unwrap_or_default())
        .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;
    let c_dst = CString::new(dst.to_str().unwrap_or_default())
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
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let dest_path = dst.join(entry.file_name());
            if path.is_dir() {
                recursive_clone_or_copy(&path, &dest_path)?;
            } else {
                let c_src = CString::new(path.to_str().unwrap_or_default())
                    .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;
                let c_dst = CString::new(dest_path.to_str().unwrap_or_default())
                    .map_err(|e| DeltaError::ClonefileError(e.to_string()))?;
                let ret = unsafe { clonefile(c_src.as_ptr(), c_dst.as_ptr(), CLONE_NOFOLLOW) };
                if ret != 0 {
                    fs::copy(&path, &dest_path)?;
                }
            }
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

/// Computes the filesystem state delta between the baseline state and ephemeral mutations.
pub fn compute_macos_fs_delta(
    baseline: &Path,
    ephemeral: &Path,
) -> Result<Vec<FsMutation>, DeltaError> {
    let mut mutations = Vec::new();

    if !ephemeral.exists() {
        return Ok(mutations);
    }

    // 1. Walk ephemeral scratch space to find Created and Modified files
    scan_ephemeral_dir(ephemeral, ephemeral, baseline, &mut mutations)?;

    // 2. Walk baseline to find Deleted files
    if baseline.exists() {
        scan_baseline_deletions(baseline, baseline, ephemeral, &mut mutations)?;
    }

    Ok(mutations)
}

fn scan_ephemeral_dir(
    current: &Path,
    ephemeral_root: &Path,
    baseline_root: &Path,
    mutations: &mut Vec<FsMutation>,
) -> Result<(), DeltaError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path
            .strip_prefix(ephemeral_root)
            .unwrap_or(&path)
            .to_path_buf();
        let baseline_path = baseline_root.join(&rel_path);

        if path.is_dir() {
            scan_ephemeral_dir(&path, ephemeral_root, baseline_root, mutations)?;
        } else {
            let metadata = entry.metadata()?;
            let size = metadata.len();
            let mode = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    metadata.mode()
                }
                #[cfg(not(unix))]
                {
                    0o644
                }
            };

            if !baseline_path.exists() {
                mutations.push(FsMutation::Created {
                    path: rel_path,
                    mode,
                    size,
                });
            } else {
                // Check if file modified
                let baseline_meta = fs::metadata(&baseline_path)?;
                if baseline_meta.len() != size || files_differ(&path, &baseline_path)? {
                    let mut orig_bytes = Vec::new();
                    let mut mut_bytes = Vec::new();
                    let _ =
                        File::open(&baseline_path).and_then(|mut f| f.read_to_end(&mut orig_bytes));
                    let _ = File::open(&path).and_then(|mut f| f.read_to_end(&mut mut_bytes));

                    let deltas = vec![ByteDelta {
                        offset: 0,
                        original: orig_bytes,
                        mutated: mut_bytes,
                    }];

                    mutations.push(FsMutation::Modified {
                        path: rel_path,
                        deltas,
                    });
                }
            }
        }
    }
    Ok(())
}

fn scan_baseline_deletions(
    current: &Path,
    baseline_root: &Path,
    ephemeral_root: &Path,
    mutations: &mut Vec<FsMutation>,
) -> Result<(), DeltaError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel_path = path
            .strip_prefix(baseline_root)
            .unwrap_or(&path)
            .to_path_buf();
        let ephemeral_path = ephemeral_root.join(&rel_path);

        if path.is_dir() {
            if ephemeral_path.exists() {
                scan_baseline_deletions(&path, baseline_root, ephemeral_root, mutations)?;
            } else {
                mutations.push(FsMutation::Deleted { path: rel_path });
            }
        } else if !ephemeral_path.exists() {
            mutations.push(FsMutation::Deleted { path: rel_path });
        }
    }
    Ok(())
}

fn files_differ(path_a: &Path, path_b: &Path) -> Result<bool, DeltaError> {
    let mut buf_a = [0u8; 4096];
    let mut buf_b = [0u8; 4096];

    let mut file_a = File::open(path_a)?;
    let mut file_b = File::open(path_b)?;

    loop {
        let read_a = file_a.read(&mut buf_a)?;
        let read_b = file_b.read(&mut buf_b)?;

        if read_a != read_b {
            return Ok(true);
        }
        if read_a == 0 {
            break;
        }
        if buf_a[..read_a] != buf_b[..read_b] {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
