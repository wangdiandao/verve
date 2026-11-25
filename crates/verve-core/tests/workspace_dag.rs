//! Verifies the Cargo workspace unidirectional dependency DAG and headless UI-isolation boundaries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

const EXPECTED_INTERNAL_EDGES: &[(&str, &[&str])] = &[
    ("verve-protocol", &[]),
    ("verve-config", &[]),
    ("verve-kernel", &[]),
    ("verve-sys", &[]),
    ("verve-kernel-mihomo", &["verve-kernel"]),
    ("verve-compiler", &["verve-config", "verve-kernel"]),
    ("verve-sync", &["verve-config"]),
    (
        "verve-core",
        &[
            "verve-protocol",
            "verve-config",
            "verve-kernel",
            "verve-kernel-mihomo",
            "verve-compiler",
            "verve-sys",
            "verve-sync",
        ],
    ),
    ("verve-service", &["verve-protocol"]),
    ("verve-ui-kit", &[]),
    ("verve-editor", &["verve-ui-kit"]),
    ("verve-app", &["verve-core", "verve-ui-kit", "verve-editor"]),
];

const HEADLESS_CRATES: &[&str] = &[
    "verve-protocol",
    "verve-config",
    "verve-kernel",
    "verve-kernel-mihomo",
    "verve-compiler",
    "verve-sys",
    "verve-sync",
    "verve-core",
    "verve-service",
];

const FORBIDDEN_UI_DEPS: &[&str] = &[
    "gpui",
    "gpui-base",
    "gpui-material",
    "verve-ui-kit",
    "verve-editor",
    "verve-app",
];

fn load_workspace_metadata() -> Value {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("verve-core must live under <workspace>/crates/verve-core")
        .join("Cargo.toml");

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest_path)
        .output()
        .expect("failed to execute `cargo metadata`");

    assert!(
        output.status.success(),
        "`cargo metadata` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("invalid JSON from `cargo metadata`")
}

#[test]
fn workspace_dag_matches_architecture_and_is_headless_safe() {
    let metadata = load_workspace_metadata();
    let packages = metadata["packages"]
        .as_array()
        .expect("`metadata.packages` must be an array");

    let expected_map: BTreeMap<&str, BTreeSet<&str>> = EXPECTED_INTERNAL_EDGES
        .iter()
        .map(|(crate_name, deps)| (*crate_name, deps.iter().copied().collect()))
        .collect();

    let mut actual_internal: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut all_deps_by_crate: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for pkg in packages {
        let name = pkg["name"]
            .as_str()
            .expect("package name must be a string")
            .to_owned();
        let deps = pkg["dependencies"]
            .as_array()
            .expect("package dependencies must be an array");

        let mut all_deps = BTreeSet::new();
        let mut internal_deps = BTreeSet::new();
        for dep in deps {
            let dep_name = dep["name"]
                .as_str()
                .expect("dependency name must be a string")
                .to_owned();
            all_deps.insert(dep_name.clone());
            if expected_map.contains_key(dep_name.as_str()) {
                internal_deps.insert(dep_name);
            }
        }

        actual_internal.insert(name.clone(), internal_deps);
        all_deps_by_crate.insert(name, all_deps);
    }

    // 1. All 12 workspace crates exist (and no unexpected workspace members).
    let actual_names: BTreeSet<&str> = actual_internal.keys().map(String::as_str).collect();
    let expected_names: BTreeSet<&str> = expected_map.keys().copied().collect();
    assert_eq!(
        actual_names, expected_names,
        "workspace crate set must match the 12 crates in docs/architecture.zh.md §3"
    );

    // 2. Internal dependency edges match docs/architecture.zh.md §3 exactly.
    for (crate_name, expected_deps) in &expected_map {
        let actual_deps: BTreeSet<&str> = actual_internal[*crate_name]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            &actual_deps, expected_deps,
            "internal dependencies for `{crate_name}` do not match architecture DAG"
        );
    }

    // Verify no reverse or cyclic edges via Kahn's topological sort.
    let mut in_degree: BTreeMap<&str, usize> =
        expected_names.iter().map(|&name| (name, 0)).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (crate_name, deps) in &actual_internal {
        for dep in deps {
            *in_degree.get_mut(crate_name.as_str()).unwrap() += 1;
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(crate_name.as_str());
        }
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter_map(|(&name, &deg)| (deg == 0).then_some(name))
        .collect();
    let mut visited = 0usize;
    while let Some(node) = queue.pop_front() {
        visited += 1;
        if let Some(children) = dependents.get(node) {
            for &child in children {
                let deg = in_degree.get_mut(child).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(child);
                }
            }
        }
    }
    assert_eq!(
        visited,
        expected_names.len(),
        "workspace internal dependency graph must be acyclic"
    );

    // 3. Headless crates must never depend on GPUI or UI crates.
    for headless in HEADLESS_CRATES {
        let deps = &all_deps_by_crate[*headless];
        for forbidden in FORBIDDEN_UI_DEPS {
            assert!(
                !deps.contains(*forbidden),
                "headless crate `{headless}` must not depend on `{forbidden}`"
            );
        }
    }
}
