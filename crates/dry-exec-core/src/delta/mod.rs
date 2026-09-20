//! Deterministic state delta engine module.

pub mod fs;
pub mod memory;
pub mod network;
pub mod types;

#[cfg(target_os = "linux")]
pub mod pagemap;

#[cfg(target_os = "macos")]
pub mod macos_fs;

pub use fs::{FsSnapshot, InodeRecord};
#[cfg(target_os = "linux")]
pub use memory::{clear_soft_dirty_bits, read_process_memory};
pub use memory::{AnonymousMemoryRegion, PAGE_SIZE};
pub use network::{MockResponse, NetworkMockSchema, TransparentProxy};
#[cfg(target_os = "linux")]
pub use pagemap::scan_dirty_pages;
pub use types::{ByteDelta, FsMutation, InterceptedRequest, PageMutation, StateDelta};

#[cfg(target_os = "macos")]
pub use macos_fs::{apfs_clone_directory, compute_macos_fs_delta};

use crate::error::DeltaError;
use std::path::Path;
use std::time::Instant;

/// Coordinator responsible for tracking and computing state deltas across boundaries.
pub struct DeltaCoordinator {
    fs_snapshot: Option<FsSnapshot>,
    network_proxy: Option<TransparentProxy>,
}

impl Default for DeltaCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaCoordinator {
    pub fn new() -> Self {
        Self {
            fs_snapshot: None,
            network_proxy: None,
        }
    }

    /// Snapshot baseline state prior to isolated process execution.
    pub fn snapshot_baseline(&mut self, fs_root: Option<&Path>) -> Result<(), DeltaError> {
        if let Some(root) = fs_root {
            self.fs_snapshot = Some(FsSnapshot::capture(root)?);
        }
        Ok(())
    }

    /// Initialize transparent network proxy on loopback interface with schema mock routes.
    pub fn start_network_proxy(&mut self, schema: NetworkMockSchema) -> Result<u16, DeltaError> {
        let proxy = TransparentProxy::start(schema)?;
        let port = proxy.port();
        self.network_proxy = Some(proxy);
        Ok(port)
    }

    /// Compute full state delta post-execution.
    pub fn compute_full_delta(
        &self,
        child_pid: i32,
        memory_region: Option<(&AnonymousMemoryRegion, usize)>,
    ) -> Result<StateDelta, DeltaError> {
        let start = Instant::now();

        // 1. Compute memory mutations (Linux: soft-dirty pagemap; macOS: empty graceful degradation)
        #[cfg(target_os = "linux")]
        let memory_mutations = if let Some((region, len)) = memory_region {
            let base_addr = region.as_ptr() as usize;
            scan_dirty_pages(child_pid, base_addr, len, region.as_slice())?
        } else {
            Vec::new()
        };

        #[cfg(target_os = "macos")]
        let memory_mutations: Vec<PageMutation> = {
            let _ = (child_pid, memory_region);
            Vec::new()
        };

        // 2. Compute filesystem mutations via snapshot comparison
        let fs_mutations = if let Some(ref snapshot) = self.fs_snapshot {
            snapshot.compute_delta()?
        } else {
            Vec::new()
        };

        // 3. Collect intercepted outbound network requests
        let network_mutations = if let Some(ref proxy) = self.network_proxy {
            proxy.drain_intercepted()
        } else {
            Vec::new()
        };

        // 4. Aggregate total bytes mutated
        let mut total_bytes = 0;
        for page in &memory_mutations {
            for delta in &page.deltas {
                total_bytes += delta.mutated.len();
            }
        }
        for fs_mut in &fs_mutations {
            match fs_mut {
                FsMutation::Created { size, .. } => total_bytes += *size as usize,
                FsMutation::Modified { deltas, .. } => {
                    for d in deltas {
                        total_bytes += d.mutated.len();
                    }
                }
                FsMutation::Deleted { .. } => {}
            }
        }
        for net_mut in &network_mutations {
            total_bytes += net_mut.request_body.len();
        }

        let duration_nanos = start.elapsed().as_nanos() as u64;

        Ok(StateDelta {
            memory_mutations,
            fs_mutations,
            network_mutations,
            total_bytes_mutated: total_bytes,
            duration_nanos,
        })
    }

    /// Compute state delta on macOS using APFS snapshot diffing and network proxy captures.
    #[cfg(target_os = "macos")]
    pub fn compute_macos_delta(
        &self,
        baseline_root: Option<&Path>,
        ephemeral_root: Option<&Path>,
    ) -> Result<StateDelta, DeltaError> {
        let start = Instant::now();
        let fs_mutations = if let (Some(base), Some(ephem)) = (baseline_root, ephemeral_root) {
            compute_macos_fs_delta(base, ephem)?
        } else if let Some(ref snapshot) = self.fs_snapshot {
            snapshot.compute_delta()?
        } else {
            Vec::new()
        };

        let network_mutations = if let Some(ref proxy) = self.network_proxy {
            proxy.drain_intercepted()
        } else {
            Vec::new()
        };

        let mut total_bytes = 0;
        for fs_mut in &fs_mutations {
            match fs_mut {
                FsMutation::Created { size, .. } => total_bytes += *size as usize,
                FsMutation::Modified { deltas, .. } => {
                    for d in deltas {
                        total_bytes += d.mutated.len();
                    }
                }
                FsMutation::Deleted { .. } => {}
            }
        }
        for net_mut in &network_mutations {
            total_bytes += net_mut.request_body.len();
        }

        let duration_nanos = start.elapsed().as_nanos() as u64;

        Ok(StateDelta {
            memory_mutations: Vec::new(),
            fs_mutations,
            network_mutations,
            total_bytes_mutated: total_bytes,
            duration_nanos,
        })
    }
}
