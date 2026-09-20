//! Type-safe data structures representing deterministic state deltas.

use std::path::PathBuf;

/// Discrete byte delta within an isolated memory page or ephemeral file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteDelta {
    /// Relative offset from start of mapped region or file.
    pub offset: usize,
    /// Pre-execution baseline bytes.
    pub original: Vec<u8>,
    /// Post-execution mutated bytes.
    pub mutated: Vec<u8>,
}

/// State mutation detected on a single virtual memory page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMutation {
    /// 0-indexed page number within the mapped memory region.
    pub page_index: usize,
    /// Starting virtual memory address of the page.
    pub page_address: usize,
    /// Detailed byte deltas detected on this page.
    pub deltas: Vec<ByteDelta>,
}

/// Ephemeral filesystem state mutation within the isolated tmpfs mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsMutation {
    /// New inode created in the execution layer.
    Created {
        path: PathBuf,
        mode: u32,
        size: u64,
    },
    /// Existing inode mutated in the execution layer.
    Modified {
        path: PathBuf,
        deltas: Vec<ByteDelta>,
    },
    /// Inode removed during execution layer lifecycle.
    Deleted {
        path: PathBuf,
    },
}

/// Discrete network request intercepted by the transparent proxy boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterceptedRequest {
    pub method: String,
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    pub request_body: Vec<u8>,
    pub response_status: u16,
    pub response_body: Vec<u8>,
}

/// Complete deterministic state delta computed post-execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDelta {
    /// Memory mutations categorized by page.
    pub memory_mutations: Vec<PageMutation>,
    /// Ephemeral filesystem mutations.
    pub fs_mutations: Vec<FsMutation>,
    /// Outbound network mutations intercepted by the transparent proxy.
    pub network_mutations: Vec<InterceptedRequest>,
    /// Total count of mutated bytes across memory and filesystem.
    pub total_bytes_mutated: usize,
    /// State delta computation duration in nanoseconds.
    pub duration_nanos: u64,
}

impl StateDelta {
    pub fn empty() -> Self {
        Self {
            memory_mutations: Vec::new(),
            fs_mutations: Vec::new(),
            network_mutations: Vec::new(),
            total_bytes_mutated: 0,
            duration_nanos: 0,
        }
    }
}
