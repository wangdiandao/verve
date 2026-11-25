//! Kernel binary allowlist, path canonicalization, SHA-256 verification, and argument hardening.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use verve_protocol::ProtocolErrorCode;

/// Allowed kernel identifier accepted by `verve-service`.
const ALLOWED_KERNEL_ID: &str = "mihomo";

/// Security policy governing which kernel binaries and directories `verve-service` may supervise.
#[derive(Debug, Clone, Default)]
pub struct SecurityPolicy {
    /// Canonical or raw directories under which kernel binaries must reside.
    pub trusted_dirs: Vec<PathBuf>,
    /// Optional lowercase/uppercase hex SHA-256 digests permitted for the kernel binary.
    ///
    /// When empty, directory membership inside [`Self::trusted_dirs`] is enforced without
    /// a fixed digest check; when non-empty, the binary digest must match one entry.
    pub allowed_sha256_hex: Vec<String>,
}

/// Validated, canonicalized arguments ready for fixed-template kernel spawning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedStartArgs {
    /// Logical kernel identifier (`"mihomo"`).
    pub kernel_id: String,
    /// Canonicalized regular-file path to the kernel executable.
    pub bin_path: PathBuf,
    /// Canonicalized working directory for the kernel process.
    pub work_dir: PathBuf,
    /// Canonicalized configuration file path passed via `-f`.
    pub config_path: PathBuf,
    /// Validated control-plane endpoint identifier.
    pub ctrl_endpoint: String,
}

/// Structured validation error mapped directly to [`ProtocolErrorCode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Machine-readable protocol error classification.
    pub code: ProtocolErrorCode,
    /// Human-readable reason.
    pub message: String,
}

impl ValidationError {
    fn invalid_arg(message: impl Into<String>) -> Self {
        Self {
            code: ProtocolErrorCode::InvalidArgument,
            message: message.into(),
        }
    }

    fn untrusted_binary(message: impl Into<String>) -> Self {
        Self {
            code: ProtocolErrorCode::UntrustedBinary,
            message: message.into(),
        }
    }
}

impl SecurityPolicy {
    /// Validates `StartKernel` inputs against the allowlist and returns canonicalized paths.
    pub fn validate_start(
        &self,
        kernel_id: &str,
        bin_path: &str,
        work_dir: &str,
        config_path: &str,
        ctrl_endpoint: &str,
    ) -> Result<ValidatedStartArgs, ValidationError> {
        if kernel_id != ALLOWED_KERNEL_ID {
            return Err(ValidationError::invalid_arg(format!(
                "unsupported kernel_id '{kernel_id}' (expected '{ALLOWED_KERNEL_ID}')"
            )));
        }

        validate_ctrl_endpoint(ctrl_endpoint)?;

        let canon_bin = self.validate_binary_path(bin_path)?;
        let canon_work_dir = validate_existing_dir(work_dir, "work_dir")?;
        let canon_config = validate_existing_file(config_path, "config_path")?;

        Ok(ValidatedStartArgs {
            kernel_id: kernel_id.to_owned(),
            bin_path: canon_bin,
            work_dir: canon_work_dir,
            config_path: canon_config,
            ctrl_endpoint: ctrl_endpoint.to_owned(),
        })
    }

    /// Validates an existing configuration file path (used by `RestartKernel`).
    pub fn validate_config_path(&self, config_path: &str) -> Result<PathBuf, ValidationError> {
        validate_existing_file(config_path, "config_path")
    }

    fn validate_binary_path(&self, bin_path: &str) -> Result<PathBuf, ValidationError> {
        if bin_path.trim().is_empty() || bin_path.contains('\0') {
            return Err(ValidationError::untrusted_binary(
                "binary path is empty or contains NUL bytes",
            ));
        }
        if self.trusted_dirs.is_empty() {
            return Err(ValidationError::untrusted_binary(
                "no trusted binary directories configured",
            ));
        }

        let canon_bin = std::fs::canonicalize(Path::new(bin_path)).map_err(|err| {
            ValidationError::untrusted_binary(format!(
                "failed to canonicalize binary path '{bin_path}': {err}"
            ))
        })?;
        let meta = std::fs::metadata(&canon_bin).map_err(|err| {
            ValidationError::untrusted_binary(format!("failed to stat binary path: {err}"))
        })?;
        if !meta.is_file() {
            return Err(ValidationError::untrusted_binary(
                "kernel binary path is not a regular file",
            ));
        }

        let inside_trusted = self.trusted_dirs.iter().any(|dir| {
            let Ok(canon_dir) = std::fs::canonicalize(dir) else {
                return false;
            };
            canon_dir.is_dir() && canon_bin != canon_dir && canon_bin.starts_with(&canon_dir)
        });
        if !inside_trusted {
            return Err(ValidationError::untrusted_binary(format!(
                "binary '{}' is outside configured trusted directories",
                canon_bin.display()
            )));
        }

        if !self.allowed_sha256_hex.is_empty() {
            let digest = compute_sha256_hex(&canon_bin).map_err(|err| {
                ValidationError::untrusted_binary(format!(
                    "failed to compute SHA-256 digest of kernel binary: {err}"
                ))
            })?;
            let matches_digest = self
                .allowed_sha256_hex
                .iter()
                .any(|allowed| allowed.trim().eq_ignore_ascii_case(&digest));
            if !matches_digest {
                return Err(ValidationError::untrusted_binary(format!(
                    "binary SHA-256 digest {digest} is not in allowlist"
                )));
            }
        }

        Ok(canon_bin)
    }
}

fn validate_ctrl_endpoint(ctrl_endpoint: &str) -> Result<(), ValidationError> {
    if ctrl_endpoint.trim().is_empty() {
        return Err(ValidationError::invalid_arg(
            "ctrl_endpoint must not be empty",
        ));
    }
    if ctrl_endpoint.chars().any(char::is_control) {
        return Err(ValidationError::invalid_arg(
            "ctrl_endpoint must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_existing_dir(raw: &str, label: &str) -> Result<PathBuf, ValidationError> {
    if raw.trim().is_empty() || raw.contains('\0') {
        return Err(ValidationError::invalid_arg(format!(
            "{label} must not be empty or contain NUL bytes"
        )));
    }
    let canon = std::fs::canonicalize(Path::new(raw)).map_err(|err| {
        ValidationError::invalid_arg(format!("{label} '{raw}' cannot be resolved: {err}"))
    })?;
    if !canon.is_dir() {
        return Err(ValidationError::invalid_arg(format!(
            "{label} '{}' is not a directory",
            canon.display()
        )));
    }
    Ok(canon)
}

fn validate_existing_file(raw: &str, label: &str) -> Result<PathBuf, ValidationError> {
    if raw.trim().is_empty() || raw.contains('\0') {
        return Err(ValidationError::invalid_arg(format!(
            "{label} must not be empty or contain NUL bytes"
        )));
    }
    let canon = std::fs::canonicalize(Path::new(raw)).map_err(|err| {
        ValidationError::invalid_arg(format!("{label} '{raw}' cannot be resolved: {err}"))
    })?;
    if !canon.is_file() {
        return Err(ValidationError::invalid_arg(format!(
            "{label} '{}' is not a regular file",
            canon.display()
        )));
    }
    Ok(canon)
}

/// Computes the lowercase hexadecimal SHA-256 digest of the file at `path`.
pub(crate) fn compute_sha256_hex(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn rejects_binary_outside_trusted_dirs_and_traversal() {
        let trusted = TempDir::new().unwrap();
        let untrusted = TempDir::new().unwrap();
        let work_dir = TempDir::new().unwrap();

        let outside_bin = untrusted.path().join("mihomo.bin");
        std::fs::write(&outside_bin, b"fake-binary").unwrap();

        let cfg = work_dir.path().join("config.yaml");
        std::fs::write(&cfg, b"port: 7890\n").unwrap();

        let policy = SecurityPolicy {
            trusted_dirs: vec![trusted.path().to_path_buf()],
            allowed_sha256_hex: Vec::new(),
        };

        // Direct outside path rejected.
        let err = policy
            .validate_start(
                "mihomo",
                outside_bin.to_str().unwrap(),
                work_dir.path().to_str().unwrap(),
                cfg.to_str().unwrap(),
                "pipe://test",
            )
            .expect_err("outside binary must be rejected");
        assert_eq!(err.code, ProtocolErrorCode::UntrustedBinary);

        // Path traversal escaping trusted_dir rejected.
        let traversal = trusted
            .path()
            .join("..")
            .join(untrusted.path().file_name().unwrap())
            .join("mihomo.bin");
        let err_traversal = policy
            .validate_start(
                "mihomo",
                traversal.to_str().unwrap(),
                work_dir.path().to_str().unwrap(),
                cfg.to_str().unwrap(),
                "pipe://test",
            )
            .expect_err("traversal binary must be rejected");
        assert_eq!(err_traversal.code, ProtocolErrorCode::UntrustedBinary);
    }

    #[test]
    fn enforces_sha256_allowlist_and_invalid_arguments() {
        let trusted = TempDir::new().unwrap();
        let work_dir = TempDir::new().unwrap();

        let bin_path = trusted.path().join("mihomo");
        let mut f = File::create(&bin_path).unwrap();
        f.write_all(b"trusted-mihomo-payload").unwrap();
        f.flush().unwrap();

        let cfg_path = work_dir.path().join("active.yaml");
        std::fs::write(&cfg_path, b"mixed-port: 7890\n").unwrap();

        let expected_hex = compute_sha256_hex(&bin_path).unwrap();

        // Wrong SHA-256 rejected.
        let bad_policy = SecurityPolicy {
            trusted_dirs: vec![trusted.path().to_path_buf()],
            allowed_sha256_hex: vec!["00".repeat(32)],
        };
        let err = bad_policy
            .validate_start(
                "mihomo",
                bin_path.to_str().unwrap(),
                work_dir.path().to_str().unwrap(),
                cfg_path.to_str().unwrap(),
                r"\\.\pipe\test",
            )
            .expect_err("mismatched sha256 must be rejected");
        assert_eq!(err.code, ProtocolErrorCode::UntrustedBinary);

        // Matching uppercase SHA-256 accepted.
        let good_policy = SecurityPolicy {
            trusted_dirs: vec![trusted.path().to_path_buf()],
            allowed_sha256_hex: vec![expected_hex.to_ascii_uppercase()],
        };
        let validated = good_policy
            .validate_start(
                "mihomo",
                bin_path.to_str().unwrap(),
                work_dir.path().to_str().unwrap(),
                cfg_path.to_str().unwrap(),
                r"\\.\pipe\test",
            )
            .expect("matching trusted binary must pass");
        assert_eq!(validated.kernel_id, "mihomo");

        // Unsupported kernel_id rejected with InvalidArgument.
        let bad_kernel = good_policy
            .validate_start(
                "evil-sh",
                bin_path.to_str().unwrap(),
                work_dir.path().to_str().unwrap(),
                cfg_path.to_str().unwrap(),
                r"\\.\pipe\test",
            )
            .expect_err("unknown kernel_id must be rejected");
        assert_eq!(bad_kernel.code, ProtocolErrorCode::InvalidArgument);
    }
}
