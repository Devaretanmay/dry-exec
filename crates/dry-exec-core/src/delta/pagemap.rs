//! Kernel page-table tracking parser utilizing /proc/[pid]/pagemap and soft-dirty flags.

use std::fs::File;
use std::os::unix::fs::FileExt;
use byteorder::{ByteOrder, LittleEndian};
use crate::delta::memory::{read_process_memory, PAGE_SIZE};
use crate::delta::types::{ByteDelta, PageMutation};
use crate::error::DeltaError;

const PAGEMAP_ENTRY_SIZE: usize = 8;
const PAGE_PRESENT_BIT: u64 = 1 << 63;
const PAGE_SOFT_DIRTY_BIT: u64 = 1 << 55;

/// Inspects /proc/[pid]/pagemap to compute deterministic memory mutations.
pub fn scan_dirty_pages(
    pid: i32,
    base_addr: usize,
    region_len: usize,
    parent_baseline: &[u8],
) -> Result<Vec<PageMutation>, DeltaError> {
    let num_pages = (region_len + PAGE_SIZE - 1) / PAGE_SIZE;
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

    for page_idx in 0..num_pages {
        let entry_offset = page_idx * PAGEMAP_ENTRY_SIZE;
        let entry = LittleEndian::read_u64(&descriptors[entry_offset..entry_offset + 8]);

        // Explicitly check Bit 63 (PAGE_PRESENT) before Bit 55 (PAGE_SOFT_DIRTY)
        let is_present = (entry & PAGE_PRESENT_BIT) != 0;
        let is_soft_dirty = (entry & PAGE_SOFT_DIRTY_BIT) != 0;

        if is_present && is_soft_dirty {
            let page_addr = base_addr + (page_idx * PAGE_SIZE);
            let region_offset = page_idx * PAGE_SIZE;
            let page_len = std::cmp::min(PAGE_SIZE, region_len.saturating_sub(region_offset));

            // Read the child's mutated page into scratch buffer
            read_process_memory(pid, page_addr, &mut child_page_buf[..page_len])?;

            // Zero-copy diffing directly against parent's mapped memory slice
            let parent_page_slice = &parent_baseline[region_offset..region_offset + page_len];
            let page_deltas = compute_slice_deltas(
                region_offset,
                parent_page_slice,
                &child_page_buf[..page_len],
            );

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
