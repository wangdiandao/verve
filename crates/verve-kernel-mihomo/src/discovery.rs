//! Discovery helpers for locating the Mihomo kernel executable.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Environment variable overriding the Mihomo binary path.
const ENV_MIHOMO_BIN: &str = "VERVE_MIHOMO_BIN";

#[cfg(windows)]
const CANDIDATE_NAMES: &[&str] = &["mihomo.exe", "verge-mihomo.exe"];

#[cfg(not(windows))]
const CANDIDATE_NAMES: &[&str] = &["mihomo", "verge-mihomo"];

#[cfg(windows)]
const STANDARD_INSTALL_PATHS: &[&str] = &[r"C:\Program Files\Clash Verge\verge-mihomo.exe"];

#[cfg(not(windows))]
const STANDARD_INSTALL_PATHS: &[&str] = &["/usr/local/bin/mihomo", "/usr/bin/mihomo"];

/// Discovers an executable Mihomo binary path according to precedence rules.
///
/// Lookup order:
/// 1. `configured` if `Some(path)` and `path.is_file()`.
/// 2. `VERVE_MIHOMO_BIN` environment variable if set and pointing to a file.
/// 3. `PATH` entries for `mihomo` or `verge-mihomo` (with `.exe` on Windows).
/// 4. Platform standard installation paths.
pub fn discover_mihomo_binary(configured: Option<&Path>) -> Option<PathBuf> {
    let env_bin = std::env::var_os(ENV_MIHOMO_BIN);
    let env_path = std::env::var_os("PATH");
    let standard_paths: Vec<&Path> = STANDARD_INSTALL_PATHS.iter().map(Path::new).collect();

    discover_mihomo_binary_with(
        configured,
        env_bin.as_deref(),
        env_path.as_deref(),
        &standard_paths,
    )
}

fn discover_mihomo_binary_with(
    configured: Option<&Path>,
    env_bin: Option<&OsStr>,
    env_path: Option<&OsStr>,
    standard_paths: &[&Path],
) -> Option<PathBuf> {
    if let Some(path) = configured
        && path.is_file()
    {
        return Some(path.to_path_buf());
    }

    if let Some(raw) = env_bin
        && !raw.is_empty()
    {
        let path = Path::new(raw);
        if path.is_file() {
            return Some(path.to_path_buf());
        }
    }

    if let Some(path_var) = env_path {
        for dir in std::env::split_paths(path_var) {
            for name in CANDIDATE_NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    for path in standard_paths {
        if path.is_file() {
            return Some((*path).to_path_buf());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestTempDir(PathBuf);

    impl TestTempDir {
        fn new(prefix: &str) -> Self {
            let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "verve-mihomo-discovery-{prefix}-{}-{seq}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path) -> PathBuf {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, b"stub").expect("write stub file");
        path.to_path_buf()
    }

    #[test]
    fn discovery_respects_precedence_order() {
        let dir = TestTempDir::new("precedence");
        let configured = touch(&dir.path().join("configured/mihomo.exe"));
        let env_bin = touch(&dir.path().join("env/mihomo.exe"));
        let path_dir = dir.path().join("path-bin");
        let path_bin = touch(&path_dir.join(CANDIDATE_NAMES[1]));
        let standard_bin = touch(&dir.path().join("standard/verge-mihomo.exe"));

        let path_env = std::env::join_paths([&path_dir]).expect("join PATH");
        let standards = [standard_bin.as_path()];

        // 1. Explicit configured path wins over everything.
        assert_eq!(
            discover_mihomo_binary_with(
                Some(&configured),
                Some(env_bin.as_os_str()),
                Some(path_env.as_os_str()),
                &standards,
            ),
            Some(configured.clone())
        );

        // 2. Non-existent configured falls through to VERVE_MIHOMO_BIN.
        let missing = dir.path().join("missing/mihomo");
        assert_eq!(
            discover_mihomo_binary_with(
                Some(&missing),
                Some(env_bin.as_os_str()),
                Some(path_env.as_os_str()),
                &standards,
            ),
            Some(env_bin)
        );

        // 3. Missing env falls through to PATH lookup (finds verge-mihomo too).
        assert_eq!(
            discover_mihomo_binary_with(
                None,
                Some(OsStr::new("")),
                Some(path_env.as_os_str()),
                &standards,
            ),
            Some(path_bin)
        );

        // 4. Empty PATH falls through to standard install paths.
        assert_eq!(
            discover_mihomo_binary_with(None, None, Some(OsStr::new("")), &standards),
            Some(standard_bin)
        );

        // 5. Directories do not count as files.
        assert_eq!(
            discover_mihomo_binary_with(
                Some(dir.path()),
                Some(OsString::from(dir.path()).as_os_str()),
                None,
                &[dir.path()],
            ),
            None
        );
    }
}
