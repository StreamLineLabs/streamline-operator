//! Hermetic tests that keep the checked-in CRD manifests in step with the Rust
//! types they are generated from.
//!
//! `deploy/crds/*.yaml` used to be hand-maintained, which meant the installed
//! schema could silently diverge from what the operator actually serialises.
//! These tests fail the build (and therefore CI and the release pipeline) as
//! soon as the two disagree. Nothing here needs a cluster, a network, or Docker.

// unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use streamline_operator::crd::generate::{
    generated_crds, render_installed_manifest, Reconciliation,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn crd_dir() -> PathBuf {
    repo_root().join("deploy").join("crds")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn checked_in_manifests_match_the_generator() {
    for crd in generated_crds().unwrap() {
        let path = crd_dir().join(&crd.file_name);
        assert!(
            path.exists(),
            "{} is missing; run `make generate-crds`",
            path.display()
        );
        assert_eq!(
            read(&path),
            crd.yaml,
            "{} is out of date; run `make generate-crds`",
            path.display()
        );
    }
}

#[test]
fn no_stray_manifests_are_shipped() {
    let expected: BTreeSet<String> = generated_crds()
        .unwrap()
        .into_iter()
        .map(|c| c.file_name)
        .chain(std::iter::once("kustomization.yaml".to_string()))
        .collect();

    let actual: BTreeSet<String> = std::fs::read_dir(crd_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        actual, expected,
        "deploy/crds contains files that the generator does not produce"
    );
}

#[test]
fn kustomization_installs_exactly_the_reconciled_crds() {
    let kustomization = read(&crd_dir().join("kustomization.yaml"));

    for crd in generated_crds().unwrap() {
        let listed = kustomization
            .lines()
            .map(str::trim)
            .any(|line| line == format!("- {}", crd.file_name));

        assert_eq!(
            listed,
            crd.is_installed(),
            "{} installed={} but kustomization listing={}",
            crd.kind,
            crd.is_installed(),
            listed
        );
    }
}

/// Every RBAC document the repository ships. The operator's default scope is a
/// namespaced Role; if a ClusterRole is ever added it must be covered by the
/// same guards, so the set is discovered from the directory rather than
/// hard-coded.
fn shipped_rbac_documents() -> Vec<(String, String)> {
    let dir = repo_root().join("deploy/rbac");
    let mut docs: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to list {}: {e}", dir.display()))
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                read(&path),
            )
        })
        .filter(|(_, body)| body.contains("kind: Role") || body.contains("kind: ClusterRole"))
        .collect();
    docs.sort();
    assert!(
        !docs.is_empty(),
        "the operator must ship at least one Role or ClusterRole"
    );
    docs
}

#[test]
fn every_installed_crd_has_matching_rbac() {
    let rbac = read(&repo_root().join("deploy/rbac/role.yaml"));

    for crd in generated_crds().unwrap() {
        // `streamlineclusters.streamline.io` -> `streamlineclusters`
        let plural = crd
            .metadata_name
            .split_once('.')
            .map(|(plural, _)| plural.to_string())
            .unwrap();

        let granted = rbac
            .lines()
            .map(str::trim)
            .any(|l| l == format!("- {plural}"));
        assert_eq!(
            granted,
            crd.is_installed(),
            "RBAC for {plural} should be granted only for reconciled CRDs"
        );

        if crd.is_installed() {
            for subresource in ["status", "finalizers"] {
                assert!(
                    rbac.lines()
                        .map(str::trim)
                        .any(|l| l == format!("- {plural}/{subresource}")),
                    "RBAC is missing {plural}/{subresource}"
                );
            }
        }
    }
}

/// The operator ships a namespaced Role by default. A ClusterRole grants the
/// operator every namespace in the cluster, which the shipped Deployment (which
/// passes an explicit `--namespace`) does not need.
#[test]
fn the_shipped_rbac_is_namespace_scoped() {
    for (name, body) in shipped_rbac_documents() {
        assert!(
            !body.contains("kind: ClusterRole"),
            "{name} ships cluster-scoped RBAC; cluster-wide mode must be opt-in"
        );
        assert!(
            body.contains("namespace: streamline-system"),
            "{name} must bind the Role to the operator's namespace"
        );
    }
    assert!(
        !repo_root().join("deploy/rbac/cluster-role.yaml").exists(),
        "the cluster-wide role must not be shipped by default"
    );
    assert!(
        !repo_root()
            .join("deploy/rbac/cluster-role-binding.yaml")
            .exists(),
        "the cluster-wide binding must not be shipped by default"
    );
}

// ---------------------------------------------------------------------------
// Unsupported kinds stay unsupported
//
// StreamlineBranch, StreamlineContract, and StreamlineMemory had controllers
// that reconciled against Streamline APIs which either do not exist or take a
// different shape. Every reconcile failed (or, for Memory, created plain topics
// and then reported the memory tiers Ready), requeued, and failed again: a hot
// loop per resource that also rewrote status on every pass.
//
// These tests make re-enabling one of them a deliberate act: a controller
// cannot be started, a CRD cannot be installed, and RBAC cannot be granted
// without first changing the reconciliation metadata in src/crd/generate.rs.
// ---------------------------------------------------------------------------

/// `StreamlineBranch` -> `BranchController`
fn controller_name(kind: &str) -> String {
    format!("{}Controller", kind.trim_start_matches("Streamline"))
}

#[test]
fn unreconciled_crds_have_no_controller_anywhere_in_the_crate() {
    let main_rs = read(&repo_root().join("src/main.rs"));
    let controllers_mod = read(&repo_root().join("src/controllers/mod.rs"));
    let lib_rs = read(&repo_root().join("src/lib.rs"));

    for crd in generated_crds().unwrap() {
        if !matches!(crd.reconciliation, Reconciliation::None(_)) {
            continue;
        }
        let controller = controller_name(crd.kind);

        assert!(
            !main_rs.contains(&controller),
            "{} has no controller, but main.rs still references {controller}",
            crd.kind
        );
        assert!(
            !controllers_mod.contains(&format!("pub use {}", controller.to_lowercase())),
            "{} has no controller, but controllers/mod.rs still exports {controller}",
            crd.kind
        );
        assert!(
            !lib_rs.contains(&controller),
            "{} has no controller, but lib.rs still re-exports {controller}",
            crd.kind
        );

        let module = crd.kind.trim_start_matches("Streamline").to_lowercase();
        let path = repo_root().join(format!("src/controllers/{module}.rs"));
        assert!(
            !path.exists(),
            "{} has no controller, but {} still exists — delete it or give the \
             kind a Reconciliation::By(..)",
            crd.kind,
            path.display()
        );
    }
}

#[test]
fn unreconciled_crds_are_not_installed_and_hold_no_rbac() {
    let kustomization = read(&crd_dir().join("kustomization.yaml"));

    for crd in generated_crds().unwrap() {
        if !matches!(crd.reconciliation, Reconciliation::None(_)) {
            continue;
        }

        assert!(
            !kustomization
                .lines()
                .map(str::trim)
                .any(|l| l == format!("- {}", crd.file_name)),
            "{} has no controller and must not be installed",
            crd.kind
        );

        let plural = crd.metadata_name.split_once('.').unwrap().0;
        for (name, body) in shipped_rbac_documents() {
            for line in body.lines().map(str::trim) {
                assert!(
                    line != format!("- {plural}")
                        && line != format!("- {plural}/status")
                        && line != format!("- {plural}/finalizers"),
                    "{name} grants {plural}, but nothing reconciles {}",
                    crd.kind
                );
            }
        }
    }
}

/// The two halves of "supported" must agree: a controller exists exactly when
/// the CRD is installed and RBAC-authorised.
#[test]
fn reconciled_crds_have_a_controller_module_and_rbac() {
    let main_rs = read(&repo_root().join("src/main.rs"));

    for crd in generated_crds().unwrap() {
        let Reconciliation::By(controller) = crd.reconciliation else {
            continue;
        };
        assert_eq!(
            controller,
            controller_name(crd.kind),
            "{} names its controller inconsistently",
            crd.kind
        );
        assert!(
            main_rs.contains(controller),
            "{controller} must be started in main.rs"
        );

        let module = crd.kind.trim_start_matches("Streamline").to_lowercase();
        assert!(
            repo_root()
                .join(format!("src/controllers/{module}.rs"))
                .exists(),
            "{} is installed but src/controllers/{module}.rs is missing",
            crd.kind
        );
    }
}

#[test]
fn short_names_are_unique_across_crds() {
    let mut seen: Vec<(String, &'static str)> = Vec::new();

    for crd in generated_crds().unwrap() {
        for line in crd.yaml.lines() {
            let trimmed = line.trim();
            if let Some(short) = trimmed.strip_prefix("- sl") {
                // Only the `shortNames:` block uses `- sl…` entries.
                let short = format!("sl{short}");
                if let Some((_, owner)) = seen.iter().find(|(s, _)| *s == short) {
                    panic!(
                        "short name `{short}` is claimed by both {owner} and {}",
                        crd.kind
                    );
                }
                seen.push((short, crd.kind));
            }
        }
    }

    assert!(!seen.is_empty(), "expected at least one short name");
}

#[test]
fn installed_manifest_matches_the_installed_files() {
    let manifest = render_installed_manifest().unwrap();

    for crd in generated_crds()
        .unwrap()
        .iter()
        .filter(|c| c.is_installed())
    {
        assert!(
            manifest.contains(&crd.yaml),
            "{} is missing from the release manifest",
            crd.kind
        );
    }
}

#[test]
fn generated_manifests_declare_the_expected_api_group() {
    for crd in generated_crds().unwrap() {
        assert!(
            crd.yaml.contains("group: streamline.io"),
            "{} must live in the streamline.io API group",
            crd.kind
        );
    }
}

#[test]
fn installed_crds_are_reconciled_by_a_controller() {
    let main_rs = read(&repo_root().join("src/main.rs"));
    for crd in generated_crds()
        .unwrap()
        .iter()
        .filter(|c| c.is_installed())
    {
        // `StreamlineCluster` -> `ClusterController`
        let controller = format!("{}Controller", crd.kind.trim_start_matches("Streamline"));
        assert!(
            main_rs.contains(&controller),
            "{} is installed but {} is not started in main.rs",
            crd.kind,
            controller
        );
    }
}
