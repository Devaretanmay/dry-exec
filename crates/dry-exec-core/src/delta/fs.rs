//! Ephemeral filesystem inode diffing engine.

use crate::delta::types::{ByteDelta, FsMutation};
use crate::error::DeltaError;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// POSIX file-type mask (`S_IFMT`) normalized to `u32` for portable mode inspection.
const S_IFMT: u32 = 0o170000;

/// POSIX directory file-type bits (`S_IFDIR`).
const S_IFDIR: u32 = 0o040000;

/// POSIX symbolic-link file-type bits (`S_IFLNK`).
const S_IFLNK: u32 = 0o120000;

/// Upper bound on bytes captured for a single modified file. Mutations beyond this prefix are
/// still applied from the scratch inode; the bound only limits the in-memory delta payload.
const MAX_DIFF_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;

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

    /// Empty baseline for an ephemeral root that starts with no durable contents.
    ///
    /// A fresh tmpfs starts empty even when the host path used as its mountpoint contains files;
    /// snapshotting that host path would report every pre-existing inode as deleted.
    pub fn empty(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            records: HashMap::new(),
        }
    }

    /// Compute deterministic filesystem state delta against post-execution state.
    pub fn compute_delta(&self) -> Result<Vec<FsMutation>, DeltaError> {
        self.compute_delta_at(&self.root)
    }

    /// Compute delta from a mounted view of the same root, such as `/proc/<pid>/root/...`.
    pub fn compute_delta_at(&self, current_root: &Path) -> Result<Vec<FsMutation>, DeltaError> {
        let mut current_records = HashMap::new();
        if current_root.exists() {
            traverse_dir(current_root, current_root, &mut current_records)?;
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
                    let current_kind = current.mode & S_IFMT;
                    let previous_kind = prev.mode & S_IFMT;
                    let previous_path = self.root.join(rel_path);
                    let current_path = current_root.join(rel_path);

                    if current_kind != previous_kind {
                        mutations.push(FsMutation::Created {
                            path: rel_path.clone(),
                            mode: current.mode,
                            size: current.size,
                        });
                        continue;
                    }

                    if current_kind == S_IFDIR {
                        continue;
                    }

                    if current_kind == S_IFLNK {
                        let previous_target =
                            std::fs::read_link(&previous_path).map_err(DeltaError::IoError)?;
                        let current_target =
                            std::fs::read_link(&current_path).map_err(DeltaError::IoError)?;
                        if previous_target != current_target {
                            mutations.push(FsMutation::Modified {
                                path: rel_path.clone(),
                                deltas: vec![ByteDelta {
                                    offset: 0,
                                    original: previous_target.to_string_lossy().as_bytes().to_vec(),
                                    mutated: current_target.to_string_lossy().as_bytes().to_vec(),
                                }],
                            });
                        }
                        continue;
                    }

                    // Clone-based trials give every cloned inode a fresh mtime, so metadata
                    // comparison would report untouched files as modified. Compare content and
                    // derive the byte payload from the retained baseline.
                    if let Some(deltas) =
                        diff_file_bytes(&previous_path, &current_path, current.size)?
                    {
                        mutations.push(FsMutation::Modified {
                            path: rel_path.clone(),
                            deltas,
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

        mutations.sort_by(|left, right| left.path().cmp(right.path()));
        Ok(mutations)
    }
}

/// Compare a baseline file with its post-execution counterpart, returning a byte delta only when
/// the content actually differs. Symbolic links are not followed, so a workspace link cannot be
/// used to read a path outside the retained baseline.
fn diff_file_bytes(
    previous: &Path,
    current: &Path,
    current_size: u64,
) -> Result<Option<Vec<ByteDelta>>, DeltaError> {
    if !is_regular_file(previous) || !is_regular_file(current) {
        return Ok(None);
    }

    let previous_size = std::fs::metadata(previous)
        .map_err(DeltaError::IoError)?
        .len();
    if previous_size == current_size
        && files_equal(previous, current).map_err(DeltaError::IoError)?
    {
        return Ok(None);
    }

    let original = read_capped(previous, MAX_DIFF_CAPTURE_BYTES)?;
    let mutated = read_capped(current, MAX_DIFF_CAPTURE_BYTES)?;
    Ok(Some(vec![ByteDelta {
        offset: 0,
        original,
        mutated,
    }]))
}

/// Whether the path is a regular file without following a terminal symbolic link.
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

/// Streaming content comparison that keeps memory bounded for large files.
fn files_equal(left: &Path, right: &Path) -> std::io::Result<bool> {
    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut left_buf = [0u8; 64 * 1024];
    let mut right_buf = [0u8; 64 * 1024];

    loop {
        let left_read = left.read(&mut left_buf)?;
        let right_read = right.read(&mut right_buf)?;
        if left_read != right_read || left_buf[..left_read] != right_buf[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

/// Read at most `limit` bytes from the start of a regular file.
fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, DeltaError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(DeltaError::IoError)?
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(DeltaError::IoError)?;
    Ok(bytes)
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
        let is_dir = (stat.mode & S_IFMT) == S_IFDIR;

        records.insert(rel_path, stat);

        if is_dir {
            traverse_dir(base_root, &path, records)?;
        }
    }

    Ok(())
}

/// Retrieve inode metadata using standard library MetadataExt.
fn stat_path(path: &Path) -> Result<InodeRecord, DeltaError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    Ok(InodeRecord {
        ino: meta.ino(),
        mode: meta.mode(),
        size: meta.size(),
        mtime_sec: meta.mtime(),
        mtime_nsec: meta.mtime_nsec() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn content_diff_reports_modified_bytes_and_ignores_identical_mtime_changes() {
        let baseline = tempdir().expect("baseline");
        let current = tempdir().expect("current");

        fs::write(baseline.path().join("changed.txt"), b"before").expect("write baseline");
        fs::write(current.path().join("changed.txt"), b"after!").expect("write current");
        fs::write(baseline.path().join("same.txt"), b"unchanged").expect("write baseline");
        fs::write(current.path().join("same.txt"), b"unchanged").expect("write current");
        fs::write(current.path().join("created.txt"), b"new").expect("write created");

        let snapshot = FsSnapshot::capture(baseline.path()).expect("snapshot");
        let mutations = snapshot
            .compute_delta_at(current.path())
            .expect("compute delta");

        let changed = mutations
            .iter()
            .find(|mutation| mutation.path() == Path::new("changed.txt"))
            .expect("changed file");
        match changed {
            FsMutation::Modified { deltas, .. } => {
                assert_eq!(deltas.len(), 1);
                assert_eq!(deltas[0].original, b"before");
                assert_eq!(deltas[0].mutated, b"after!");
            }
            other => panic!("expected modified, got {other:?}"),
        }

        assert!(
            !mutations
                .iter()
                .any(|mutation| mutation.path() == Path::new("same.txt")),
            "unchanged content must not be reported as modified"
        );
        assert!(mutations.iter().any(|mutation| {
            mutation.path() == Path::new("created.txt")
                && matches!(mutation, FsMutation::Created { .. })
        }));
    }

    #[test]
    fn content_diff_reports_deletions_against_baseline() {
        let baseline = tempdir().expect("baseline");
        let current = tempdir().expect("current");
        fs::write(baseline.path().join("removed.txt"), b"gone").expect("write baseline");

        let snapshot = FsSnapshot::capture(baseline.path()).expect("snapshot");
        let mutations = snapshot
            .compute_delta_at(current.path())
            .expect("compute delta");

        assert!(mutations.iter().any(|mutation| {
            mutation.path() == Path::new("removed.txt")
                && matches!(mutation, FsMutation::Deleted { .. })
        }));
    }

    #[test]
    fn empty_snapshot_reports_child_files_as_created() {
        let current = tempdir().expect("current");
        fs::write(current.path().join("ephemeral.txt"), b"data").expect("write");

        let snapshot = FsSnapshot::empty(Path::new("/nonexistent"));
        let mutations = snapshot
            .compute_delta_at(current.path())
            .expect("compute delta");

        assert!(mutations.iter().any(|mutation| {
            mutation.path() == Path::new("ephemeral.txt")
                && matches!(mutation, FsMutation::Created { size: 4, .. })
        }));
    }
    #[test]
    fn content_diff_reports_type_changes_and_symlink_retargets() {
        let baseline = tempdir().expect("baseline");
        let current = tempdir().expect("current");

        fs::write(baseline.path().join("replaced.txt"), b"file").expect("write baseline");
        std::os::unix::fs::symlink("target", current.path().join("replaced.txt")).expect("link");
        std::os::unix::fs::symlink("old", baseline.path().join("moved_link")).expect("link");
        std::os::unix::fs::symlink("new", current.path().join("moved_link")).expect("link");

        let snapshot = FsSnapshot::capture(baseline.path()).expect("snapshot");
        let mutations = snapshot
            .compute_delta_at(current.path())
            .expect("compute delta");

        assert!(mutations.iter().any(|mutation| {
            mutation.path() == Path::new("replaced.txt")
                && matches!(mutation, FsMutation::Created { .. })
        }));

        let moved = mutations
            .iter()
            .find(|mutation| mutation.path() == Path::new("moved_link"))
            .expect("retargeted symlink");
        match moved {
            FsMutation::Modified { deltas, .. } => {
                assert_eq!(deltas[0].original, b"old");
                assert_eq!(deltas[0].mutated, b"new");
            }
            other => panic!("expected modified, got {other:?}"),
        }
    }
}
