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
    Created { path: PathBuf, mode: u32, size: u64 },
    /// Existing inode mutated in the execution layer.
    Modified {
        path: PathBuf,
        deltas: Vec<ByteDelta>,
    },
    /// Inode removed during execution layer lifecycle.
    Deleted { path: PathBuf },
}

impl FsMutation {
    /// Target path of the filesystem mutation.
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::Created { path, .. } => path.as_path(),
            Self::Modified { path, .. } => path.as_path(),
            Self::Deleted { path } => path.as_path(),
        }
    }

    /// Identifies whether the mutation targets persistent execution hooks outside the turn perimeter.
    ///
    /// Detects mutations targeting `.git/hooks/*`, shell initialization files (`.bashrc`,
    /// `.zshrc`, `.profile`), Python site hooks (`*.pth`, `sitecustomize.py`), and virtualenv
    /// activation scripts that would trigger post-turn execution outside the isolated boundary.
    pub fn is_deferred_execution_hazard(&self) -> bool {
        let path = self.path();
        let path_str = path.to_string_lossy();

        // 1. Git hooks
        if path_str.contains(".git/hooks/") || path_str.ends_with(".git/hooks") {
            return true;
        }

        // 2. Shell initialization scripts and Python site hooks
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            match file_name {
                ".bashrc" | ".bash_profile" | ".bash_login" | ".profile" | ".zshrc"
                | ".zprofile" | ".zshenv" | ".zlogin" | ".bash_logout" | ".zlogout" => return true,
                "sitecustomize.py" | "usercustomize.py" => return true,
                _ => {}
            }
            if file_name.ends_with(".pth") {
                return true;
            }
        }

        // 3. Virtual environment activation hooks
        if path_str.ends_with("/bin/activate")
            || path_str.ends_with("/bin/activate.sh")
            || path_str.ends_with("/bin/activate.csh")
            || path_str.ends_with("/bin/activate.fish")
            || path_str.ends_with("/Scripts/activate.bat")
            || path_str.ends_with("/Scripts/Activate.ps1")
        {
            return true;
        }

        false
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateDelta {
    /// Memory mutations categorized by page.
    pub memory_mutations: Vec<PageMutation>,
    /// Ephemeral filesystem mutations.
    pub fs_mutations: Vec<FsMutation>,
    /// Outbound network mutations intercepted by the transparent proxy.
    pub network_mutations: Vec<InterceptedRequest>,
    /// Count of intercepted routes the transparent proxy refused outside the registered schema.
    /// Precomputed here so the decision layer can route a categorical refusal from a scalar.
    pub schema_breaches: usize,
    /// Total count of mutated bytes across memory and filesystem.
    pub total_bytes_mutated: usize,
    /// State delta computation duration in nanoseconds.
    pub duration_nanos: u64,
    /// Captured stdout emitted by the isolated action.
    pub stdout: Vec<u8>,
    /// Captured stderr emitted by the isolated action.
    pub stderr: Vec<u8>,
    /// Exit status returned by the isolated action.
    pub exit_code: i32,
}

impl StateDelta {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deferred_execution_hazard_detection() {
        let hook_mutation = FsMutation::Created {
            path: PathBuf::from(".git/hooks/pre-commit"),
            mode: 0o755,
            size: 120,
        };
        assert!(hook_mutation.is_deferred_execution_hazard());

        let bashrc_mutation = FsMutation::Modified {
            path: PathBuf::from("/home/user/.bashrc"),
            deltas: vec![],
        };
        assert!(bashrc_mutation.is_deferred_execution_hazard());

        let pth_mutation = FsMutation::Created {
            path: PathBuf::from("site-packages/backdoor.pth"),
            mode: 0o644,
            size: 64,
        };
        assert!(pth_mutation.is_deferred_execution_hazard());

        let activate_mutation = FsMutation::Modified {
            path: PathBuf::from(".venv/bin/activate"),
            deltas: vec![],
        };
        assert!(activate_mutation.is_deferred_execution_hazard());

        let normal_mutation = FsMutation::Created {
            path: PathBuf::from("src/main.rs"),
            mode: 0o644,
            size: 512,
        };
        assert!(!normal_mutation.is_deferred_execution_hazard());
    }
}
