//! Copy-on-Write (CoW) memory mapping and cross-boundary inspection primitives.

use crate::error::DeltaError;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::ptr::NonNull;

pub const PAGE_SIZE: usize = 4096;

/// Ephemeral anonymous memory region backed by Copy-on-Write kernel mappings.
#[derive(Debug)]
pub struct AnonymousMemoryRegion {
    ptr: NonNull<u8>,
    len: usize,
}

// Memory mapping can be shared across control plane threads safely.
unsafe impl Send for AnonymousMemoryRegion {}
unsafe impl Sync for AnonymousMemoryRegion {}

impl AnonymousMemoryRegion {
    /// Allocate an anonymous memory region mapped with MAP_PRIVATE.
    pub fn allocate(len: usize) -> Result<Self, DeltaError> {
        if len == 0 {
            return Err(DeltaError::MmapFailure(
                "Allocation length must be greater than zero".into(),
            ));
        }

        // Align length to page size boundary
        let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                aligned_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(DeltaError::MmapFailure(format!(
                "mmap MAP_PRIVATE failed: errno {}",
                std::io::Error::last_os_error()
            )));
        }

        // Byte-precise delta tracking requires 4KB page-table granularity: a transparent huge
        // page marks an entire 2MB range soft-dirty for a single byte mutation, which defeats
        // the O(P_dirty) scan bound by forcing reads of untouched pages.
        #[cfg(target_os = "linux")]
        unsafe {
            let _ = libc::madvise(ptr, aligned_len, libc::MADV_NOHUGEPAGE);
        }

        Ok(Self {
            ptr: NonNull::new(ptr as *mut u8)
                .ok_or_else(|| DeltaError::MmapFailure("mmap returned NULL pointer".into()))?,
            len: aligned_len,
        })
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for AnonymousMemoryRegion {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::munmap(self.ptr.as_ptr() as *mut libc::c_void, self.len);
        }
    }
}

/// Reset kernel page-table soft-dirty bits for target process PID.
#[cfg(target_os = "linux")]
pub fn clear_soft_dirty_bits(pid: i32) -> Result<(), DeltaError> {
    let clear_refs_path = format!("/proc/{pid}/clear_refs");
    let mut file = OpenOptions::new()
        .write(true)
        .open(&clear_refs_path)
        .map_err(DeltaError::IoError)?;

    file.write_all(b"4\n").map_err(DeltaError::IoError)?;
    Ok(())
}

/// Read virtual memory from target process address space into destination buffer.
#[cfg(target_os = "linux")]
pub fn read_process_memory(
    pid: i32,
    remote_addr: usize,
    dest: &mut [u8],
) -> Result<(), DeltaError> {
    let local_iov = libc::iovec {
        iov_base: dest.as_mut_ptr() as *mut libc::c_void,
        iov_len: dest.len(),
    };

    let remote_iov = libc::iovec {
        iov_base: remote_addr as *mut libc::c_void,
        iov_len: dest.len(),
    };

    let res = unsafe {
        libc::process_vm_readv(
            pid,
            &local_iov as *const libc::iovec,
            1,
            &remote_iov as *const libc::iovec,
            1,
            0,
        )
    };

    if res >= 0 && res as usize == dest.len() {
        return Ok(());
    }

    // Fallback: Read directly from /proc/[pid]/mem
    let mem_path = format!("/proc/{pid}/mem");
    let file = OpenOptions::new().read(true).open(&mem_path).map_err(|e| {
        DeltaError::ProcessMemoryError {
            pid,
            reason: format!("Failed to open {mem_path}: {e}"),
        }
    })?;

    use std::os::unix::fs::FileExt;
    file.read_exact_at(dest, remote_addr as u64)
        .map_err(|e| DeltaError::ProcessMemoryError {
            pid,
            reason: format!("Failed to read at offset {remote_addr:#x}: {e}"),
        })?;

    Ok(())
}
