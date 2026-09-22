//! Kernel page-table tracking parser utilizing /proc/[pid]/pagemap and soft-dirty flags.

use crate::delta::memory::{read_process_memory, PAGE_SIZE};
use crate::delta::types::{ByteDelta, PageMutation};
use crate::error::DeltaError;
use std::fs::File;
use std::os::unix::fs::FileExt;

const PAGEMAP_ENTRY_SIZE: usize = 8;
const PAGE_PRESENT_BIT: u64 = 1 << 63;
const PAGE_SOFT_DIRTY_BIT: u64 = 1 << 55;
const PAGE_EXCLUSIVE_BIT: u64 = 1 << 56;

/// Inspects /proc/[pid]/pagemap to compute deterministic memory mutations.
pub fn scan_dirty_pages(
    pid: i32,
    base_addr: usize,
    region_len: usize,
    parent_baseline: &[u8],
) -> Result<Vec<PageMutation>, DeltaError> {
    let num_pages = region_len.div_ceil(PAGE_SIZE);
    let pagemap_path = format!("/proc/{pid}/pagemap");
    let pagemap_file = File::open(&pagemap_path).map_err(|e| {
        DeltaError::PagemapParseError(format!("Failed to open {pagemap_path}: {e}"))
    })?;

    // Byte offset within pagemap corresponding to starting virtual page
    let start_page_index = base_addr / PAGE_SIZE;
    let pagemap_offset = (start_page_index * PAGEMAP_ENTRY_SIZE) as u64;

    // Read all 64-bit descriptors for this memory region
    let total_bytes = num_pages * PAGEMAP_ENTRY_SIZE;
    let mut descriptors = vec![0u8; total_bytes];
    pagemap_file
        .read_exact_at(&mut descriptors, pagemap_offset)
        .map_err(|e| {
            DeltaError::PagemapParseError(format!("Failed to read pagemap descriptors: {e}"))
        })?;

    let mut mutations = Vec::new();
    let mut child_page_buf = [0u8; PAGE_SIZE];
    let local_pid = std::process::id() as i32 == pid;
    let soft_dirty_available = descriptors.chunks_exact(PAGEMAP_ENTRY_SIZE).any(|bytes| {
        let entry = u64::from_le_bytes(bytes.try_into().unwrap());
        entry & PAGE_SOFT_DIRTY_BIT != 0
    });

    for page_idx in 0..num_pages {
        let entry_offset = page_idx * PAGEMAP_ENTRY_SIZE;
        let entry = u64::from_le_bytes(
            descriptors[entry_offset..entry_offset + 8]
                .try_into()
                .unwrap(),
        );

        // Explicitly check Bit 63 (PAGE_PRESENT) before Bit 55 (PAGE_SOFT_DIRTY)
        let is_present = (entry & PAGE_PRESENT_BIT) != 0;
        let is_soft_dirty = (entry & PAGE_SOFT_DIRTY_BIT) != 0;
        // Some hardened kernels expose the exclusive/COW bit while masking soft-dirty. In a
        // forked boundary, a private page becomes exclusive exactly when the child writes it.
        // Use that signal only when the kernel exposes no soft-dirty bits at all.
        let is_exclusive = (entry & PAGE_EXCLUSIVE_BIT) != 0;
        let candidate = is_soft_dirty || (!soft_dirty_available && is_exclusive);

        if is_present && candidate {
            let page_addr = base_addr + (page_idx * PAGE_SIZE);
            let region_offset = page_idx * PAGE_SIZE;
            let page_len = std::cmp::min(PAGE_SIZE, region_len.saturating_sub(region_offset));

            // Zero-copy diffing directly against parent's mapped memory slice
            let parent_page_slice = &parent_baseline[region_offset..region_offset + page_len];
            let page_deltas = if local_pid {
                // Same-process benchmark path: avoid one process_vm_readv syscall per page.
                let local_page =
                    unsafe { std::slice::from_raw_parts(page_addr as *const u8, page_len) };
                compute_slice_deltas(region_offset, parent_page_slice, local_page)
            } else {
                read_process_memory(pid, page_addr, &mut child_page_buf[..page_len])?;
                compute_slice_deltas(
                    region_offset,
                    parent_page_slice,
                    &child_page_buf[..page_len],
                )
            };

            if !page_deltas.is_empty() {
                mutations.push(PageMutation {
                    page_index: page_idx,
                    page_address: page_addr,
                    deltas: page_deltas,
                });
            }
        }
    }

    Ok(mutations)
}

/// Compute contiguous byte differences between baseline slice and child buffer.
fn compute_slice_deltas(
    page_region_offset: usize,
    baseline: &[u8],
    mutated: &[u8],
) -> Vec<ByteDelta> {
    if baseline.len() == mutated.len()
        && baseline.len() > 0
        && unsafe {
            libc::memcmp(
                baseline.as_ptr() as *const libc::c_void,
                mutated.as_ptr() as *const libc::c_void,
                baseline.len(),
            )
        } == 0
    {
        return Vec::new();
    }

    let mut deltas = Vec::new();
    let mut in_diff = false;
    let mut diff_start = 0;

    for i in 0..baseline.len() {
        if baseline[i] != mutated[i] {
            if !in_diff {
                in_diff = true;
                diff_start = i;
            }
        } else if in_diff {
            in_diff = false;
            deltas.push(ByteDelta {
                offset: page_region_offset + diff_start,
                original: baseline[diff_start..i].to_vec(),
                mutated: mutated[diff_start..i].to_vec(),
            });
        }
    }

    if in_diff {
        let i = baseline.len();
        deltas.push(ByteDelta {
            offset: page_region_offset + diff_start,
            original: baseline[diff_start..i].to_vec(),
            mutated: mutated[diff_start..i].to_vec(),
        });
    }

    deltas
}
