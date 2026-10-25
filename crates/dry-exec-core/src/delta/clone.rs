//! Copy-on-write workspace cloning used to materialize ephemeral trials.

use crate::error::DeltaError;
use std::path::Path;

/// Reject destinations that overlap the source tree. Removing or cloning into an ancestor or
/// descendant would otherwise delete the source before the clone starts.
pub(super) fn reject_overlap(src: &Path, dst: &Path) -> Result<(), DeltaError> {
    if !src.exists() {
        return Ok(());
    }
    let src_canonical = std::fs::canonicalize(src).map_err(DeltaError::IoError)?;
    // Resolve the existing prefix even when several destination parents do not yet exist.
    // Falling back to a relative lexical path would bypass comparison with canonical src.
    let absolute_dst = std::path::absolute(dst).map_err(DeltaError::IoError)?;
    let existing = absolute_dst
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| {
            DeltaError::ClonefileError("clone destination has no existing ancestor".to_string())
        })?;
    let resolved = std::fs::canonicalize(existing)
        .map_err(DeltaError::IoError)?
        .join(
            absolute_dst
                .strip_prefix(existing)
                .map_err(|error| DeltaError::ClonefileError(error.to_string()))?,
        );
    let mut dst_canonical = std::path::PathBuf::new();
    for component in resolved.components() {
        match component {
            std::path::Component::ParentDir => {
                dst_canonical.pop();
            }
            std::path::Component::CurDir => {}
            other => dst_canonical.push(other.as_os_str()),
        }
    }
    if dst_canonical.starts_with(&src_canonical) || src_canonical.starts_with(&dst_canonical) {
        return Err(DeltaError::ClonefileError(
            "clone destination overlaps the source tree".to_string(),
        ));
    }
    Ok(())
}

/// Clone a directory tree without materializing a full byte copy where the filesystem supports
/// reflinks. The destination must not be an ancestor or descendant of the source.
#[cfg(target_os = "macos")]
pub fn clone_directory(src: &Path, dst: &Path) -> Result<(), DeltaError> {
    crate::delta::macos_fs::apfs_clone_directory(src, dst)
}

/// Linux cannot clone a whole tree with one syscall; clone each regular file with `FICLONE` and
/// fall back to a bounded copy only when the filesystem does not support reflinks.
#[cfg(target_os = "linux")]
pub fn clone_directory(src: &Path, dst: &Path) -> Result<(), DeltaError> {
    use std::ffi::CString;
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;

    const FICLONE: libc::c_ulong = 0x4004_9409;

    fn clone_file(src: &Path, dst: &Path, mode: u32) -> io::Result<()> {
        let mut source = File::open(src)?;
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode & 0o7777)
            .open(dst)?;
        let reflinked =
            unsafe { libc::ioctl(target.as_raw_fd(), FICLONE, source.as_raw_fd()) } == 0;
        if !reflinked {
            io::copy(&mut source, &mut target)?;
        }
        Ok(())
    }

    fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(src)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            let target = fs::read_link(src)?;
            let c_target = CString::new(target.as_os_str().as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let c_dst = CString::new(dst.as_os_str().as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let created = unsafe { libc::symlink(c_target.as_ptr(), c_dst.as_ptr()) };
            if created != 0 {
                return Err(io::Error::last_os_error());
            }
        } else if file_type.is_dir() {
            fs::create_dir(dst)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                clone_tree(&entry.path(), &dst.join(entry.file_name()))?;
            }
            fs::set_permissions(dst, fs::Permissions::from_mode(metadata.mode() & 0o7777))?;
        } else if file_type.is_file() {
            clone_file(src, dst, metadata.mode())?;
        }

        Ok(())
    }

    if !src.exists() {
        std::fs::create_dir_all(dst).map_err(DeltaError::IoError)?;
        return Ok(());
    }
    reject_overlap(src, dst)?;

    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(DeltaError::IoError)?;
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(DeltaError::IoError)?;
    }

    clone_tree(src, dst).map_err(DeltaError::IoError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn overlap_check_resolves_missing_relative_parents() {
        let source = tempfile::tempdir_in(".").unwrap();
        let relative_source = source
            .path()
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap();
        let destination = relative_source.join("not_created/nested");
        assert!(reject_overlap(source.path(), &destination).is_err());
        assert!(source.path().exists());
    }

    #[test]
    fn clone_directory_preserves_tree_without_mutating_source() {
        let source = tempdir().expect("source");
        let target = tempdir().expect("target");
        let clone = target.path().join("workspace");

        fs::create_dir(source.path().join("nested")).expect("mkdir");
        fs::write(source.path().join("nested/file.txt"), b"baseline").expect("write");
        #[cfg(unix)]
        std::os::unix::fs::symlink("nested/file.txt", source.path().join("link")).expect("link");

        clone_directory(source.path(), &clone).expect("clone directory");

        assert_eq!(
            fs::read(clone.join("nested/file.txt")).expect("read clone"),
            b"baseline"
        );
        fs::write(clone.join("nested/file.txt"), b"mutated").expect("mutate clone");
        assert_eq!(
            fs::read(source.path().join("nested/file.txt")).expect("read source"),
            b"baseline"
        );
        assert!(fs::symlink_metadata(clone.join("link"))
            .expect("link metadata")
            .file_type()
            .is_symlink());
    }
    #[test]
    fn clone_directory_rejects_overlapping_destination() {
        let source = tempdir().expect("source");
        fs::create_dir(source.path().join("nested")).expect("mkdir");
        fs::write(source.path().join("nested/keep.txt"), b"keep").expect("write");

        let result = clone_directory(source.path(), &source.path().join("nested"));
        assert!(result.is_err());
        assert_eq!(
            fs::read(source.path().join("nested/keep.txt")).expect("read"),
            b"keep"
        );
    }

    #[test]
    fn clone_directory_handles_read_only_trees() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let source = tempdir().expect("source");
        let target = tempdir().expect("target");
        let clone = target.path().join("workspace");

        let ro_dir = source.path().join("readonly_dir");
        fs::create_dir(&ro_dir).expect("mkdir");
        let ro_file = ro_dir.join("readonly_file.txt");
        fs::write(&ro_file, b"readonly content").expect("write");
        #[cfg(unix)]
        {
            fs::set_permissions(&ro_file, fs::Permissions::from_mode(0o444)).expect("chmod file");
            fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o555)).expect("chmod dir");
        }

        clone_directory(source.path(), &clone).expect("clone directory");
        assert_eq!(
            fs::read(clone.join("readonly_dir/readonly_file.txt")).expect("read clone"),
            b"readonly content"
        );

        // Restore permissions so tempdir cleanup succeeds
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o755));
            let _ = fs::set_permissions(
                clone.join("readonly_dir"),
                fs::Permissions::from_mode(0o755),
            );
        }
    }
}
