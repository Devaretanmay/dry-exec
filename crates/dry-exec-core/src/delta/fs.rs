//! Ephemeral filesystem inode diffing engine.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use crate::delta::types::FsMutation;
use crate::error::DeltaError;

/// Minimal inode metadata snapshot for efficient delta evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InodeRecord {
    pub ino: u64,
    pub mode: u32,
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: u32,
}

/// Filesystem snapshot containing inode records indexed by relative path.
#[derive(Debug, Clone)]
pub struct FsSnapshot {
    pub root: PathBuf,
    pub records: HashMap<PathBuf, InodeRecord>,
}

impl FsSnapshot {
    /// Capture baseline inode metadata across the designated root directory.
    pub fn capture(root: &Path) -> Result<Self, DeltaError> {
        let mut records = HashMap::new();
        if root.exists() {
            traverse_dir(root, root, &mut records)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            records,
        })
    }

    /// Compute deterministic filesystem state delta against post-execution state.
    pub fn compute_delta(&self) -> Result<Vec<FsMutation>, DeltaError> {
        let mut current_records = HashMap::new();
        if self.root.exists() {
            traverse_dir(&self.root, &self.root, &mut current_records)?;
        }

        let mut mutations = Vec::new();

        // 1. Detect Created and Modified inodes
        for (rel_path, current) in &current_records {
            match self.records.get(rel_path) {
                None => {
                    mutations.push(FsMutation::Created {
                        path: rel_path.clone(),
                        mode: current.mode,
                        size: current.size,
                    });
                }
                Some(prev) => {
                    // Classified as Modified only if inode is identical but mtime or size changed
                    if prev.ino == current.ino {
                        if prev.size != current.size
                            || prev.mtime_sec != current.mtime_sec
                            || prev.mtime_nsec != current.mtime_nsec
                        {
                            mutations.push(FsMutation::Modified {
                                path: rel_path.clone(),
                                deltas: Vec::new(),
                            });
                        }
                    } else {
                        // Replaced inode counts as recreation
                        mutations.push(FsMutation::Created {
                            path: rel_path.clone(),
                            mode: current.mode,
                            size: current.size,
                        });
                    }
                }
            }
        }

        // 2. Detect Deleted inodes
        for rel_path in self.records.keys() {
            if !current_records.contains_key(rel_path) {
                mutations.push(FsMutation::Deleted {
                    path: rel_path.clone(),
                });
            }
        }

        Ok(mutations)
    }
}

/// Recursively traverses directory using low-level stat calls for efficiency.
fn traverse_dir(
    base_root: &Path,
    current_dir: &Path,
    records: &mut HashMap<PathBuf, InodeRecord>,
) -> Result<(), DeltaError> {
    let entries = std::fs::read_dir(current_dir).map_err(DeltaError::IoError)?;

    for entry in entries {
        let entry = entry.map_err(DeltaError::IoError)?;
        let path = entry.path();
        let rel_path = path
            .strip_prefix(base_root)
            .map_err(|e| DeltaError::FsDiffError(e.to_string()))?
            .to_path_buf();

        let stat = stat_path(&path)?;
        let is_dir = (stat.mode & libc::S_IFMT) == libc::S_IFDIR;

        records.insert(rel_path, stat);

        if is_dir {
            traverse_dir(base_root, &path, records)?;
        }
    }

    Ok(())
}

/// Retrieve inode metadata using raw libc statx or stat.
fn stat_path(path: &Path) -> Result<InodeRecord, DeltaError> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| {
        DeltaError::FsDiffError(format!("Invalid path conversion {}: {e}", path.display()))
    })?;

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let res = unsafe { libc::lstat(c_path.as_ptr(), &mut st) };
    if res != 0 {
        return Err(DeltaError::IoError(std::io::Error::last_os_error()));
    }

    Ok(InodeRecord {
        ino: st.st_ino,
        mode: st.st_mode,
        size: st.st_size as u64,
        mtime_sec: st.st_mtime,
        mtime_nsec: st.st_mtime_nsec as u32,
    })
}
