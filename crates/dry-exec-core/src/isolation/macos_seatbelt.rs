//! macOS Seatbelt (sandbox_init) kernel isolation primitive.

use crate::error::IsolationError;
use std::ffi::{CStr, CString};
use std::path::PathBuf;

extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;
    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}

/// Configuration for dynamic SBPL (Sandbox Profile Language) generation.
#[derive(Debug, Clone)]
pub struct SeatbeltConfig {
    pub allowed_read_paths: Vec<PathBuf>,
    pub allowed_write_paths: Vec<PathBuf>,
    pub allowed_exec_paths: Vec<PathBuf>,
    pub allow_loopback_network: bool,
    pub loopback_proxy_port: Option<u16>,
    pub allow_process_exec: bool,
}

impl Default for SeatbeltConfig {
    fn default() -> Self {
        Self {
            allowed_read_paths: vec![
                PathBuf::from("/usr"),
                PathBuf::from("/System"),
                PathBuf::from("/Library"),
                PathBuf::from("/opt/homebrew"),
                PathBuf::from("/dev"),
                PathBuf::from("/etc"),
                PathBuf::from("/private/etc"),
                PathBuf::from("/private/tmp"),
                PathBuf::from("/private/var"),
                PathBuf::from("/var"),
            ],
            allowed_write_paths: vec![PathBuf::from("/private/tmp/dex_ephemeral")],
            allowed_exec_paths: vec![
                PathBuf::from("/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/opt/homebrew"),
                PathBuf::from("/Library/Developer"),
            ],
            allow_loopback_network: true,
            loopback_proxy_port: None,
            allow_process_exec: true,
        }
    }
}

/// Escape a path as SBPL string content so configuration cannot add profile forms.
fn escape_sbpl_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Generates a strict, deterministic SBPL (Sandbox Profile Language) scheme.
pub fn generate_seatbelt_profile(config: &SeatbeltConfig) -> String {
    let mut sbpl = String::new();
    sbpl.push_str("(version 1)\n");
    sbpl.push_str("(deny default)\n\n");

    // Standard compute and process capabilities
    if config.allow_process_exec {
        sbpl.push_str(";; Process execution within ephemeral boundary\n");
        sbpl.push_str("(allow process-exec (literal \"/bin/echo\"))\n");
        sbpl.push_str("(allow process-exec (literal \"/bin/sh\"))\n");
        sbpl.push_str("(allow process-exec (literal \"/usr/bin/python3\"))\n");
        for path in &config.allowed_exec_paths {
            sbpl.push_str(&format!(
                "(allow process-exec (subpath \"{}\"))\n",
                escape_sbpl_string(&path.to_string_lossy())
            ));
        }
        sbpl.push_str("(allow process-fork)\n");
        sbpl.push_str("(allow sysctl-read)\n\n");
        sbpl.push_str("(allow signal (target self))\n");
        sbpl.push('\n');
    }

    // Terminal and standard descriptor IO
    sbpl.push_str(";; Terminal and standard device descriptors\n");
    sbpl.push_str("(allow file-read-data (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/urandom\") (literal \"/dev/dtracehelper\"))\n");
    sbpl.push_str("(allow file-read* (literal \"/dev/null\"))\n");
    sbpl.push_str("(allow file-read* (literal \"/dev/urandom\"))\n");
    // dyld reads root directory entry during process startup.
    sbpl.push_str("(allow file-read-data (literal \"/\"))\n");
    sbpl.push_str("(allow file-read-metadata)\n");
    sbpl.push_str("(allow file-write-data (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\"))\n");
    sbpl.push_str("(allow file-ioctl (literal \"/dev/dtracehelper\") (literal \"/dev/null\"))\n\n");

    // Read paths
    sbpl.push_str(";; Allowed read paths\n");
    for path in &config.allowed_read_paths {
        sbpl.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl_string(&path.to_string_lossy())
        ));
    }
    for path in &config.allowed_exec_paths {
        sbpl.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl_string(&path.to_string_lossy())
        ));
    }
    sbpl.push_str("(allow file-read* (literal \"/bin/echo\"))\n");
    sbpl.push_str("(allow file-read* (literal \"/bin/sh\"))\n");
    sbpl.push_str("(allow file-read* (literal \"/usr/bin/python3\"))\n");
    sbpl.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
    sbpl.push_str("(allow file-read* (subpath \"/System/Library\"))\n");
    sbpl.push('\n');

    // Write paths strictly restricted to ephemeral scratch directory
    sbpl.push_str(";; Allowed write paths strictly restricted to ephemeral scratch space\n");
    for path in &config.allowed_write_paths {
        sbpl.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            escape_sbpl_string(&path.to_string_lossy())
        ));
        sbpl.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl_string(&path.to_string_lossy())
        ));
    }
    sbpl.push('\n');

    // Network isolation: only loopback permitted for transparent proxy
    if let Some(port) = config
        .loopback_proxy_port
        .filter(|_| config.allow_loopback_network)
    {
        sbpl.push_str(";; Loopback network egress confined to transparent mock proxy\n");
        sbpl.push_str(&format!(
            "(allow network-outbound (to ip \"localhost:{port}\"))\n"
        ));
    }

    // Explicit credential protections
    sbpl.push_str(";; Explicit denial of credential and secret locations\n");
    if let Ok(home) = std::env::var("HOME") {
        let home = escape_sbpl_string(&home);
        sbpl.push_str(&format!("(deny file-read* (subpath \"{home}/.ssh\"))\n"));
        sbpl.push_str(&format!("(deny file-read* (subpath \"{home}/.aws\"))\n"));
        sbpl.push_str(&format!("(deny file-read* (subpath \"{home}/.gnupg\"))\n"));
        sbpl.push_str(&format!("(deny file-read* (subpath \"{home}/.docker\"))\n"));
        sbpl.push_str(&format!("(deny file-read* (subpath \"{home}/.kube\"))\n"));
        sbpl.push_str(&format!(
            "(deny file-read* (subpath \"{home}/.config/gcloud\"))\n"
        ));
        sbpl.push_str(&format!("(deny file-read* (literal \"{home}/.netrc\"))\n"));
    }

    sbpl
}

/// Applies the compiled SBPL Seatbelt profile to the calling process.
/// This call is irreversible for the process and all child threads/processes.
pub fn apply_seatbelt_profile(profile: &str) -> Result<(), IsolationError> {
    let c_profile = CString::new(profile)
        .map_err(|e| IsolationError::SeatbeltFailure(format!("Invalid CString profile: {e}")))?;

    let mut err_ptr: *mut libc::c_char = std::ptr::null_mut();

    let ret = unsafe { sandbox_init(c_profile.as_ptr(), 0, &mut err_ptr) };
    if ret != 0 {
        let err_msg = if !err_ptr.is_null() {
            let msg = unsafe { CStr::from_ptr(err_ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(err_ptr) };
            msg
        } else {
            format!("sandbox_init returned error code {ret}")
        };
        return Err(IsolationError::SeatbeltFailure(err_msg));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_paths_cannot_inject_profile_rules() {
        let config = SeatbeltConfig {
            allowed_write_paths: vec![PathBuf::from("/tmp/path\"))\n(allow default)\n;")],
            ..SeatbeltConfig::default()
        };
        let profile = generate_seatbelt_profile(&config);
        assert!(!profile.lines().any(|line| line == "(allow default)"));
        assert!(profile.contains("path\\\"))\\n(allow default)\\n;"));
    }

    #[test]
    fn test_seatbelt_profile_syntax_and_denials() {
        let config = SeatbeltConfig::default();
        let profile = generate_seatbelt_profile(&config);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow process-exec (literal \"/bin/echo\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/private/tmp/dex_ephemeral\"))"));
        assert!(!profile.contains("(allow network-outbound"));
        assert!(profile.contains(".ssh"));
    }

    #[test]
    fn network_access_is_limited_to_the_registered_proxy_port() {
        let mut config = SeatbeltConfig {
            loopback_proxy_port: Some(31415),
            ..SeatbeltConfig::default()
        };
        let profile = generate_seatbelt_profile(&config);
        assert!(profile.contains("(allow network-outbound (to ip \"localhost:31415\"))"));
        assert!(!profile.contains("localhost:*"));
        assert!(!profile.contains("(allow network-bind"));
        config.allow_loopback_network = false;
        assert!(!generate_seatbelt_profile(&config).contains("(allow network-outbound"));
    }
}
