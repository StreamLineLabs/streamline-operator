//! Hermetic checks over the repository's static assets: Dockerfile, workflows,
//! Kubernetes manifests, and the claims made in the README.
//!
//! These replace the "someone will notice in review" class of release blockers
//! (a builder image behind the MSRV, probes pointing at paths nothing serves,
//! documented API groups that do not exist) with a failing `cargo test`.
//! Nothing here needs a cluster, a network, Docker, or extra tooling.

// unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path: PathBuf = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {relative}: {e}"))
}

fn exists(relative: &str) -> bool {
    Path::new(&repo_root().join(relative)).exists()
}

/// Parse a `major.minor[.patch]` version into comparable parts.
fn version_parts(version: &str) -> (u32, u32, u32) {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn declared_msrv() -> String {
    read("Cargo.toml")
        .lines()
        .find_map(|l| l.strip_prefix("rust-version = "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("Cargo.toml must declare rust-version")
}

// ---------------------------------------------------------------------------
// Toolchain
// ---------------------------------------------------------------------------

#[test]
fn dockerfile_builder_satisfies_declared_msrv() {
    let dockerfile = read("Dockerfile");
    let builder = dockerfile
        .lines()
        .find(|l| l.starts_with("FROM rust:"))
        .expect("Dockerfile must build with a rust image");

    let image_version = builder
        .trim_start_matches("FROM rust:")
        .split(['-', ' '])
        .next()
        .expect("rust image must carry a version tag");

    assert!(
        version_parts(image_version) >= version_parts(&declared_msrv()),
        "Dockerfile builds with rust:{image_version} but Cargo.toml requires {}",
        declared_msrv()
    );
}

#[test]
fn ci_pins_the_declared_msrv() {
    let ci = read(".github/workflows/ci.yml");
    let msrv = declared_msrv();
    assert!(
        ci.contains(&format!("toolchain: \"{msrv}\"")),
        "CI must build with the declared MSRV {msrv}"
    );
}

// ---------------------------------------------------------------------------
// Operator deployment
// ---------------------------------------------------------------------------

#[test]
fn operator_probes_target_endpoints_the_operator_serves() {
    let deployment = read("deploy/operator.yaml");
    let health = read("src/health.rs");

    for path in ["/healthz", "/readyz"] {
        assert!(
            deployment.contains(&format!("path: {path}")),
            "operator.yaml must probe {path}"
        );
        assert!(
            health.contains(&format!("\"{path}\"")),
            "the probe server must serve {path}"
        );
    }
}

#[test]
fn health_probe_server_starts_before_waiting_for_leadership() {
    let main = read("src/main.rs");
    let health_server = main
        .find("streamline_operator::health::router")
        .expect("main.rs must start the health probe server");
    let lease_wait = main
        .find("elector.acquire().await")
        .expect("main.rs must acquire the leader lease");

    assert!(
        health_server < lease_wait,
        "standby replicas must serve /healthz and /readyz while waiting for the leader lease"
    );
}

/// A leader-election standby must pass `/readyz`.
///
/// Readiness used to be set only after the controllers started, i.e. only on
/// the leader. Under the shipped `maxUnavailable: 1` rolling update that
/// deadlocks an HA Deployment: the new pod blocks on the lease the outgoing
/// pod still holds, and the rollout blocks on the new pod becoming ready.
#[test]
fn the_process_is_marked_ready_before_it_blocks_on_the_lease() {
    let main = read("src/main.rs");

    let client_built = main
        .find("Client::try_default()")
        .expect("main.rs must build a Kubernetes client");
    let probe_server = main
        .find("streamline_operator::health::router")
        .expect("main.rs must start the probe server");
    let mark_ready = main
        .find("readiness.mark_ready()")
        .expect("main.rs must mark the process ready");
    let lease_wait = main
        .find("elector.acquire().await")
        .expect("main.rs must acquire the leader lease");

    assert!(
        client_built < mark_ready && probe_server < mark_ready,
        "readiness must only be claimed after the client and probe server exist"
    );
    assert!(
        mark_ready < lease_wait,
        "a standby must be ready while it waits for the lease, or rolling \
         updates of an HA deployment cannot complete"
    );
}

/// Leadership is separate state, set only once the lease is actually held.
#[test]
fn leadership_is_recorded_only_after_the_lease_is_acquired() {
    let main = read("src/main.rs");

    let lease_wait = main
        .find("elector.acquire().await")
        .expect("main.rs must acquire the leader lease");
    let mark_leader = main
        .find("readiness.mark_leader()")
        .expect("main.rs must record leadership");

    assert!(
        lease_wait < mark_leader,
        "leadership must not be claimed before the lease is held"
    );
    assert!(
        main.contains("mark_standby()"),
        "losing the lease must demote the replica to standby"
    );
}

/// Readiness and leadership must be observable separately.
#[test]
fn readiness_and_leadership_have_distinct_endpoints() {
    let health = read("src/health.rs");
    let deployment = read("deploy/operator.yaml");

    for path in ["/healthz", "/readyz", "/leaderz"] {
        assert!(
            health.contains(&format!("\"{path}\"")),
            "the probe server must serve {path}"
        );
    }

    assert!(
        deployment.contains("path: /readyz"),
        "the readiness probe must stay on /readyz"
    );
    assert!(
        !deployment.contains("path: /leaderz"),
        "/leaderz must not gate pod readiness: only one replica is ever the leader"
    );
    assert!(
        read("src/metrics.rs").contains("streamline_operator_leader "),
        "leadership must also be exported as a metric"
    );
}

// ---------------------------------------------------------------------------
// Release image gating
//
// deploy/ used to ship `ghcr.io/streamlinelabs/streamline-operator:0.3.0` (in
// both operator.yaml and the kustomize `images:` override). That image predates
// this working tree, so `kubectl apply -k deploy/` quietly ran an *older*
// operator than the CRDs, RBAC, and manifests it was applied with. The
// checked-in default is now a valid reference that no registry serves, so the
// mistake becomes an ImagePullBackOff instead of a silently wrong operator.
// ---------------------------------------------------------------------------

/// The placeholder tag the repository ships until a release is cut.
const IMAGE_PLACEHOLDER: &str = "REPLACE_WITH_RELEASED_IMAGE";

/// The operator image repository, without a tag or digest.
const OPERATOR_IMAGE_REPO: &str = "ghcr.io/streamlinelabs/streamline-operator";

/// Every `image:` value in a manifest.
fn image_references(yaml: &str) -> Vec<String> {
    yaml.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("image: "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .collect()
}

#[test]
fn the_checked_in_operator_image_is_an_unpublished_placeholder() {
    let deployment = read("deploy/operator.yaml");
    let images = image_references(&deployment);

    assert_eq!(images.len(), 1, "operator.yaml must run exactly one image");
    let image = &images[0];

    assert_eq!(
        *image,
        format!("{OPERATOR_IMAGE_REPO}:{IMAGE_PLACEHOLDER}"),
        "the checked-in manifest must carry the unpublished placeholder"
    );

    // A valid reference: `repo:tag`, tag matching [A-Za-z0-9_][A-Za-z0-9._-]*
    let (repo, tag) = image.rsplit_once(':').expect("image must carry a tag");
    assert_eq!(repo, OPERATOR_IMAGE_REPO);
    assert!(tag.len() <= 128, "tag exceeds the OCI 128-character limit");
    assert!(
        tag.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_'),
        "tag `{tag}` is not a valid OCI tag"
    );
    assert!(
        tag.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'),
        "tag `{tag}` is not a valid OCI tag"
    );
}

#[test]
fn the_placeholder_image_cannot_be_satisfied_from_a_node_cache() {
    let deployment = read("deploy/operator.yaml");
    assert!(
        deployment.contains("imagePullPolicy: Always"),
        "the placeholder must be pulled every start, or a stale cached layer \
         could run an old operator anyway"
    );
}

#[test]
fn no_manifest_pins_a_released_operator_image() {
    for relative in ["deploy/operator.yaml", "deploy/kustomization.yaml"] {
        let body = read(relative);
        for line in body.lines() {
            // Comments explain the placeholder and may mention the old tag.
            let code = line.split('#').next().unwrap_or_default();
            assert!(
                !code.contains(&format!("{OPERATOR_IMAGE_REPO}:0.")),
                "{relative} pins a released operator image: {line}"
            );
            assert!(
                !code.contains("newTag"),
                "{relative} overrides the image tag, re-introducing a published \
                 image the placeholder exists to prevent: {line}"
            );
        }
    }
}

/// Releasing requires an explicit, immutable image *and* tag/version equality.
#[test]
fn the_release_workflow_substitutes_an_immutable_image() {
    let release = read(".github/workflows/release.yml");

    assert!(
        release.contains(IMAGE_PLACEHOLDER),
        "the release must replace the manifest placeholder"
    );
    assert!(
        release.contains("steps.build.outputs.digest") || release.contains("@sha256:"),
        "the release manifest must reference an immutable digest"
    );
    assert!(
        release.contains("Tag v$TAG != Cargo.toml $CARGO_VERSION")
            || release.contains("CARGO_VERSION"),
        "the release must verify the tag matches the Cargo.toml version"
    );
    assert!(
        release.contains("org.opencontainers.image.version"),
        "the release must verify the image labels carry the released version"
    );
    assert!(
        release.contains("tr '[:upper:]' '[:lower:]'"),
        "the release must normalize github.repository before constructing an OCI image name"
    );
    assert!(
        release.contains("steps.image.outputs.repository"),
        "the normalized image repository must be reused for metadata, verification, and manifest rendering"
    );
}

/// `make release-manifests` is the documented way to render a deployable
/// manifest, and it must refuse to run without an explicit image.
#[test]
fn the_release_manifest_target_requires_an_explicit_image() {
    let makefile = read("Makefile");

    assert!(
        makefile.contains("release-manifests:"),
        "the Makefile must expose a release-manifests target"
    );
    let target = makefile
        .split("\nrelease-manifests:")
        .nth(1)
        .expect("release-manifests target body");
    // Recipe lines are tab-indented; the target ends at the next line that
    // starts in column 0.
    let body: String = target
        .lines()
        .skip(1)
        .take_while(|l| l.starts_with('\t') || l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !body.trim().is_empty(),
        "release-manifests must have a recipe"
    );
    assert!(
        body.contains("IMAGE"),
        "release-manifests must take an IMAGE argument: {body}"
    );
    assert!(
        body.contains("exit 1"),
        "release-manifests must fail closed when IMAGE is missing: {body}"
    );
    assert!(
        body.contains(IMAGE_PLACEHOLDER) || makefile.contains("IMAGE_PLACEHOLDER"),
        "release-manifests must substitute the placeholder: {body}"
    );
}

#[test]
fn the_release_manifest_check_verifies_version_agreement() {
    let makefile = read("Makefile");
    assert!(
        makefile.contains("verify-release:"),
        "the Makefile must expose a verify-release target"
    );
    let cargo_version = read("Cargo.toml")
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("Cargo.toml must declare a version");
    assert!(
        !cargo_version.is_empty(),
        "the crate version must be readable by the release check"
    );
}

#[test]
fn operator_bind_address_args_parse_as_socket_addresses() {
    let deployment = read("deploy/operator.yaml");

    let addresses: Vec<&str> = deployment
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- --"))
        .filter(|l| {
            l.starts_with("metrics-bind-address=") || l.starts_with("health-probe-bind-address=")
        })
        .filter_map(|l| l.split_once('='))
        .map(|(_, addr)| addr)
        .collect();

    assert_eq!(
        addresses.len(),
        2,
        "operator.yaml must configure both bind addresses"
    );
    for address in addresses {
        assert!(
            address.parse::<std::net::SocketAddr>().is_ok(),
            "`{address}` is not a bindable socket address"
        );
    }
}

#[test]
fn operator_manifests_reference_the_same_namespace() {
    let namespace = read("deploy/namespace.yaml");
    assert!(namespace.contains("streamline-system"));
    assert!(read("deploy/operator.yaml").contains("namespace: streamline-system"));
    assert!(read("deploy/kustomization.yaml").contains("namespace: streamline-system"));
}

#[test]
fn manifests_parse_as_yaml() {
    for relative in [
        "deploy/namespace.yaml",
        "deploy/operator.yaml",
        "deploy/kustomization.yaml",
        "deploy/crds/kustomization.yaml",
        "deploy/rbac/role.yaml",
        "deploy/rbac/role-binding.yaml",
        "deploy/rbac/service-account.yaml",
        "overlays/cloud/kustomization.yaml",
        "overlays/cloud/operator-watch-all.yaml",
        "overlays/cloud/cluster-role.yaml",
        "overlays/cloud/cluster-role-binding.yaml",
        "overlays/cloud/leader-election-role.yaml",
        "overlays/cloud/control-plane-namespace.yaml",
    ] {
        let content = read(relative);
        serde_yaml::from_str::<serde_yaml::Value>(&content)
            .unwrap_or_else(|e| panic!("{relative} is not valid YAML: {e}"));
    }
}

/// Every Role/ClusterRole this repository ships, as (file name, YAML).
fn shipped_roles() -> Vec<(String, String)> {
    let dir = repo_root().join("deploy/rbac");
    let mut roles: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to list {}: {e}", dir.display()))
        .map(|entry| {
            let path = entry.unwrap().path();
            let body = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                body,
            )
        })
        .filter(|(_, body)| {
            let doc: serde_yaml::Value =
                serde_yaml::from_str(body).expect("RBAC manifest must be valid YAML");
            matches!(doc["kind"].as_str(), Some("Role" | "ClusterRole"))
        })
        .collect();
    for relative in [
        "overlays/cloud/cluster-role.yaml",
        "overlays/cloud/leader-election-role.yaml",
    ] {
        roles.push((relative.to_string(), read(relative)));
    }
    roles.sort();
    assert!(!roles.is_empty(), "expected at least one shipped role");
    roles
}

fn role_rules(yaml: &str) -> Vec<serde_yaml::Value> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml).expect("role must be valid YAML");
    doc.get("rules")
        .and_then(|rules| rules.as_sequence())
        .cloned()
        .expect("role must declare rules")
}

/// The `rules:` a role actually grants, parsed rather than grepped so a
/// commented-out rule cannot be mistaken for a live one.
fn granted_resources(yaml: &str) -> Vec<String> {
    let mut resources = Vec::new();
    for rule in role_rules(yaml) {
        if let Some(list) = rule.get("resources").and_then(|r| r.as_sequence()) {
            for entry in list {
                if let Some(name) = entry.as_str() {
                    resources.push(name.to_string());
                }
            }
        }
    }
    resources
}

/// The operator must hold no Secret permissions at all.
///
/// It never calls the Secret API: TLS material named by a StreamlineCluster is
/// mounted through a `SecretVolumeSource`, which the kubelet reads on the
/// pod's behalf and which needs no operator RBAC, and user credentials are
/// never provisioned because user management is unsupported. A read/list/watch
/// grant therefore bought nothing and turned a compromised operator into a
/// secret reader for every namespace it was bound to.
#[test]
fn no_shipped_role_grants_secrets() {
    for (name, body) in shipped_roles() {
        let resources = granted_resources(&body);
        assert!(
            !resources.iter().any(|r| r == "secrets"),
            "{name} grants `secrets`, but the operator never calls the Secret API: {resources:?}"
        );
    }
}

/// Namespaces are cluster-scoped: a namespaced Role cannot grant them, and the
/// operator does not need them (it reads its own namespace from the projected
/// service account token).
#[test]
fn no_shipped_role_grants_cluster_scoped_resources() {
    for (name, body) in shipped_roles() {
        let resources = granted_resources(&body);
        for cluster_scoped in ["namespaces", "nodes", "customresourcedefinitions"] {
            assert!(
                !resources.iter().any(|r| r == cluster_scoped),
                "{name} grants cluster-scoped `{cluster_scoped}`"
            );
        }
    }
}

/// The operator deletes stale HPAs on every cluster reconcile, so the grant it
/// relies on must actually be there.
#[test]
fn the_operator_can_delete_the_hpas_it_cleans_up() {
    let role = read("deploy/rbac/role.yaml");
    let doc: serde_yaml::Value = serde_yaml::from_str(&role).expect("role must be valid YAML");
    let rules = doc
        .get("rules")
        .and_then(|r| r.as_sequence())
        .expect("role must declare rules");

    let hpa_rule = rules
        .iter()
        .find(|rule| {
            rule.get("resources")
                .and_then(|r| r.as_sequence())
                .is_some_and(|list| {
                    list.iter()
                        .any(|e| e.as_str() == Some("horizontalpodautoscalers"))
                })
        })
        .expect("the operator must be able to manage HPAs it owns");

    let verbs: Vec<&str> = hpa_rule
        .get("verbs")
        .and_then(|v| v.as_sequence())
        .expect("HPA rule must declare verbs")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(
        verbs.contains(&"delete"),
        "HPA cleanup needs `delete`, got {verbs:?}"
    );
}

// ---------------------------------------------------------------------------
// Documentation claims
// ---------------------------------------------------------------------------

#[test]
fn readme_uses_the_real_api_group() {
    let readme = read("README.md");
    assert!(
        !readme.contains("streaming.streamlinelabs.dev"),
        "README documents an API group the CRDs do not use"
    );
    assert!(
        readme.contains("streamline.io/v1alpha1"),
        "README must document the streamline.io API group"
    );
}

#[test]
fn readme_does_not_promise_an_unpublished_helm_chart() {
    let readme = read("README.md");
    assert!(
        !readme.contains("helm repo add streamline"),
        "README must not document a Helm repository that is not published"
    );
    assert!(
        !readme.contains("helm install streamline-operator streamline/"),
        "README must not document installing an unpublished chart"
    );
}

/// A Markdown table row, and where it is.
///
/// Rows are recognised the way a Markdown renderer recognises them: a line
/// whose first non-space character is `|`.
fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// A row that only *looks* like a table separator (`|---|---|`).
fn is_table_separator(line: &str) -> bool {
    is_table_row(line)
        && line.trim().trim_matches('|').split('|').all(|cell| {
            !cell.trim().is_empty() && cell.trim().chars().all(|c| c == '-' || c == ':')
        })
}

/// Table rows that prose (or a blank line) has separated from their table,
/// reported as `(line number, row)`.
///
/// Markdown ends a table at the first line that is not part of it, so rows
/// written after an intervening paragraph are not rendered as table rows at
/// all — they appear verbatim, pipes and all, and the settings they document
/// silently drop out of the table a reader is scanning. A row is legitimate
/// only if it continues the table above it or is a header immediately followed
/// by a separator line.
fn orphaned_table_rows(markdown: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut orphans = Vec::new();
    let mut in_table = false;
    let mut in_code_fence = false;

    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            in_code_fence = !in_code_fence;
            in_table = false;
            continue;
        }
        if in_code_fence {
            continue;
        }

        if !is_table_row(line) {
            // Anything else — prose or a blank line — closes the table.
            in_table = false;
            continue;
        }
        if in_table {
            continue;
        }

        // The first row of a table must be a header followed by a separator.
        let starts_a_table = lines
            .get(index + 1)
            .is_some_and(|next| is_table_separator(next));
        if starts_a_table {
            in_table = true;
        } else {
            orphans.push((index + 1, (*line).to_string()));
        }
    }

    orphans
}

/// Documentation tables must not be split by prose.
///
/// `docs/ENVIRONMENT.md` listed `--generate-crds` and `--generate-crds-dir` as
/// table rows placed *after* two explanatory paragraphs, so both flags
/// rendered as literal pipe-delimited text below the flag table instead of
/// inside it. The flags were documented and invisible at the same time.
#[test]
fn documentation_tables_are_not_split_by_prose() {
    let mut failures: Vec<String> = Vec::new();

    for source in [
        "README.md",
        "docs/API.md",
        "docs/ENVIRONMENT.md",
        "docs/UPGRADING.md",
    ] {
        for (line, row) in orphaned_table_rows(&read(source)) {
            failures.push(format!(
                "{source}:{line} is a table row separated from its table, so it renders \
                 as literal text: {}",
                row.trim()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "documentation tables are split by prose:\n  {}",
        failures.join("\n  ")
    );
}

/// A guard on the guard: the detector must still catch the exact shape that
/// `docs/ENVIRONMENT.md` shipped, and must not flag well-formed tables.
#[test]
fn the_split_table_check_rejects_a_row_stranded_after_a_paragraph() {
    let split = "\
| Flag | Description |
|------|-------------|
| `--namespace` | Namespace to watch |

Some prose that ends the table.
| `--generate-crds` | Print the CRDs and exit |
";
    let orphans = orphaned_table_rows(split);
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    assert_eq!(orphans[0].0, 6);
    assert!(orphans[0].1.contains("--generate-crds"));

    let intact = "\
| Flag | Description |
|------|-------------|
| `--namespace` | Namespace to watch |
| `--generate-crds` | Print the CRDs and exit |

Some prose after the table.

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | tracing filter |
";
    assert!(
        orphaned_table_rows(intact).is_empty(),
        "consecutive tables separated by prose are valid Markdown"
    );

    // A pipe inside a fenced block is not a table row.
    let fenced = "\
```text
| not | a | table |
```
";
    assert!(orphaned_table_rows(fenced).is_empty());
}

/// Every CLI flag the operator accepts must appear in the flag table, not
/// merely somewhere in the file: a flag documented in prose is a flag readers
/// scanning the table will miss.
#[test]
fn every_operator_flag_appears_in_the_environment_flag_table() {
    let environment = read("docs/ENVIRONMENT.md");
    let rows: Vec<&str> = environment
        .lines()
        .filter(|line| is_table_row(line) && !is_table_separator(line))
        .collect();

    for flag in [
        "--namespace",
        "--leader-election",
        "--leader-election-namespace",
        "--metrics-bind-address",
        "--health-probe-bind-address",
        "--generate-crds",
        "--generate-crds-dir",
    ] {
        assert!(
            rows.iter().any(|row| row.contains(&format!("`{flag}"))),
            "docs/ENVIRONMENT.md documents `{flag}` outside the flag table"
        );
    }
}

#[test]
fn readme_documents_the_actual_cli_flags() {
    let readme = read("README.md");
    let main_rs = read("src/main.rs");

    for flag in ["--metrics-bind-address", "--health-probe-bind-address"] {
        assert!(
            main_rs.contains(&flag.trim_start_matches("--").replace('-', "_")),
            "main.rs must expose {flag}"
        );
    }
    for stale in ["--metrics-port", "--health-port"] {
        assert!(
            !readme.contains(stale),
            "README documents `{stale}`, which the CLI does not accept"
        );
    }
}

// ---------------------------------------------------------------------------
// Release and supply-chain workflows
// ---------------------------------------------------------------------------

#[test]
fn release_crd_generation_fails_closed() {
    let release = read(".github/workflows/release.yml");
    assert!(
        !release.contains("CRD generation not yet implemented"),
        "the release must not swallow CRD generation failures"
    );
    assert!(
        !release.contains("continue-on-error: true"),
        "release steps must not continue on error"
    );
    assert!(
        release.contains("--generate-crds"),
        "the release must generate CRDs from the binary"
    );
    assert!(
        release.contains("git diff --exit-code"),
        "the release must verify the checked-in CRDs match the generator"
    );
}

#[test]
fn codeql_does_not_analyse_this_repository_as_cpp() {
    let codeql = read(".github/workflows/codeql.yml");
    assert!(
        !codeql.contains("languages: cpp"),
        "this repository contains no C++; analysing it as cpp produces no findings"
    );
    assert!(
        !codeql.contains("/language:cpp"),
        "stale cpp category left in the CodeQL workflow"
    );
}

#[test]
fn supply_chain_scanning_is_configured() {
    assert!(
        exists(".github/workflows/security.yml"),
        "a cargo-audit/cargo-deny workflow must exist"
    );
    let security = read(".github/workflows/security.yml");
    assert!(security.contains("cargo-deny") || security.contains("cargo deny"));
    assert!(security.contains("audit"));
    assert!(exists("deny.toml"), "cargo-deny needs deny.toml");
}

#[test]
fn dependabot_watches_every_ecosystem_in_use() {
    let dependabot = read(".github/dependabot.yml");
    for ecosystem in ["cargo", "github-actions", "docker"] {
        assert!(
            dependabot.contains(&format!("package-ecosystem: \"{ecosystem}\"")),
            "dependabot must watch the {ecosystem} ecosystem"
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-file consistency
// ---------------------------------------------------------------------------

#[test]
fn default_broker_image_matches_the_integration_server_image() {
    let crd = read("src/crd/cluster.rs");
    let integration = read("tests/integration.rs");

    let default_image = crd
        .lines()
        .skip_while(|l| !l.contains("fn default_image()"))
        .find_map(|l| l.trim().strip_prefix('"'))
        .map(|l| l.split('"').next().unwrap_or_default().to_string())
        .expect("default_image must return a literal");

    assert!(
        integration.contains(&default_image),
        "the default broker image {default_image} is not the server image the \
         integration harness exercises"
    );
    assert!(
        !default_image.contains("streamline-operator"),
        "brokers must not default to the operator image"
    );
}

#[test]
fn broker_readiness_probe_path_is_documented_once() {
    let controller = read("src/controllers/cluster.rs");
    assert!(
        controller.contains("\"/health/ready\""),
        "the broker readiness probe must target /health/ready"
    );
    assert!(
        !controller.contains("Some(\"/ready\".to_string())"),
        "the stale /ready probe path is still rendered"
    );
}

// ---------------------------------------------------------------------------
// Namespace scope
// ---------------------------------------------------------------------------

/// `--namespace` used to be accepted, logged, and then ignored: every
/// controller called `Api::all`. The operator therefore watched (and needed
/// RBAC for) the whole cluster no matter what the flag said.
#[test]
fn every_enabled_controller_resolves_its_watch_through_the_shared_scope() {
    for controller in ["cluster", "topic", "user"] {
        let source = read(&format!("src/controllers/{controller}.rs"));
        assert!(
            source.contains("self.scope.api(self.client.clone())"),
            "{controller}.rs must resolve its Api through the shared WatchScope"
        );
        assert!(
            !source.contains("Api::all("),
            "{controller}.rs must not hard-code a cluster-wide watch"
        );
    }
}

#[test]
fn the_shipped_deployment_watches_a_single_namespace() {
    let deployment = read("deploy/operator.yaml");
    let namespace_args: Vec<&str> = deployment
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- --namespace="))
        .collect();

    assert_eq!(
        namespace_args.len(),
        1,
        "the deployment must pass exactly one --namespace"
    );
    assert!(
        !namespace_args[0].is_empty(),
        "an empty --namespace is the cluster-wide mode, which the shipped \
         namespaced Role cannot authorise"
    );
    assert_eq!(
        namespace_args[0], "$(OPERATOR_NAMESPACE)",
        "the watch must follow the namespace the operator is deployed into"
    );
    assert!(
        deployment.contains("name: OPERATOR_NAMESPACE"),
        "$(OPERATOR_NAMESPACE) must be defined in the container env for \
         Kubernetes to expand it"
    );
}

#[test]
fn the_cloud_overlay_explicitly_watches_every_namespace() {
    let overlay = read("overlays/cloud/kustomization.yaml");
    assert!(
        overlay.lines().any(|line| line.trim() == "- ../../deploy"),
        "the cloud overlay must extend the default install rather than fork it"
    );
    for resource in [
        "cluster-role.yaml",
        "cluster-role-binding.yaml",
        "leader-election-role.yaml",
        "control-plane-namespace.yaml",
    ] {
        assert!(
            overlay.lines().any(|line| {
                line.trim() == format!("- {resource}")
                    || line.trim() == format!("- path: {resource}")
            }),
            "the cloud overlay must include {resource}"
        );
    }

    let patch: serde_yaml::Value =
        serde_yaml::from_str(&read("overlays/cloud/operator-watch-all.yaml"))
            .expect("cloud Deployment patch must be valid YAML");
    let container = patch["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .and_then(|containers| {
            containers
                .iter()
                .find(|container| container["name"].as_str() == Some("operator"))
        })
        .expect("cloud patch must target the operator container");
    let namespace_args: Vec<&str> = container["args"]
        .as_sequence()
        .expect("cloud patch must replace the operator args")
        .iter()
        .filter_map(|arg| arg.as_str())
        .filter(|arg| arg.starts_with("--namespace="))
        .collect();

    assert_eq!(
        namespace_args,
        vec!["--namespace="],
        "cloud mode must explicitly select WatchScope::AllNamespaces"
    );

    let namespace_patch: serde_yaml::Value =
        serde_yaml::from_str(&read("overlays/cloud/control-plane-namespace.yaml"))
            .expect("cloud Namespace patch must be valid YAML");
    assert_eq!(namespace_patch["kind"].as_str(), Some("Namespace"));
    assert_eq!(
        namespace_patch["metadata"]["name"].as_str(),
        Some("streamline-system")
    );
    assert_eq!(
        namespace_patch["metadata"]["labels"]["streamline.io/control-plane"].as_str(),
        Some("true"),
        "cloud mode must label the operator namespace so tenant NetworkPolicies \
         admit its HTTP 9094 reconciliation traffic"
    );

    let base_namespace: serde_yaml::Value =
        serde_yaml::from_str(&read("deploy/namespace.yaml")).expect("base Namespace is valid YAML");
    assert!(
        base_namespace["metadata"]["labels"]["streamline.io/control-plane"].is_null(),
        "the cross-namespace access label must remain opt-in with the cloud overlay"
    );

    let default = read("deploy/kustomization.yaml");
    assert!(
        !default.contains("overlays/cloud") && !default.contains("cluster-role.yaml"),
        "cluster-wide RBAC must remain opt-in; deploy/ is the namespaced default"
    );
}

#[test]
fn the_cloud_overlay_grants_only_reconciliation_rules_cluster_wide() {
    let base_rules = role_rules(&read("deploy/rbac/role.yaml"));
    let grants_lease = |rule: &serde_yaml::Value| {
        rule.get("resources")
            .and_then(|resources| resources.as_sequence())
            .is_some_and(|resources| {
                resources
                    .iter()
                    .any(|resource| resource.as_str() == Some("leases"))
            })
    };

    let expected_cluster_rules: Vec<serde_yaml::Value> = base_rules
        .iter()
        .filter(|rule| !grants_lease(rule))
        .cloned()
        .collect();
    let expected_lease_rules: Vec<serde_yaml::Value> = base_rules
        .iter()
        .filter(|rule| grants_lease(rule))
        .cloned()
        .collect();

    assert_eq!(
        role_rules(&read("overlays/cloud/cluster-role.yaml")),
        expected_cluster_rules,
        "the cloud ClusterRole must match the default reconciliation permissions \
         exactly, without widening leader-election access"
    );
    assert_eq!(
        role_rules(&read("overlays/cloud/leader-election-role.yaml")),
        expected_lease_rules,
        "the cloud overlay must keep Lease permissions in streamline-system"
    );

    let binding: serde_yaml::Value =
        serde_yaml::from_str(&read("overlays/cloud/cluster-role-binding.yaml"))
            .expect("cloud ClusterRoleBinding must be valid YAML");
    assert_eq!(binding["roleRef"]["kind"].as_str(), Some("ClusterRole"));
    assert_eq!(
        binding["roleRef"]["name"].as_str(),
        Some("streamline-operator-cloud")
    );
    let subject = binding["subjects"]
        .as_sequence()
        .and_then(|subjects| subjects.first())
        .expect("cloud ClusterRoleBinding must name its ServiceAccount");
    assert_eq!(subject["kind"].as_str(), Some("ServiceAccount"));
    assert_eq!(subject["name"].as_str(), Some("streamline-operator"));
    assert_eq!(subject["namespace"].as_str(), Some("streamline-system"));
}

#[test]
fn streamline_custom_resource_verbs_are_least_privilege_and_match_both_modes() {
    for relative in ["deploy/rbac/role.yaml", "overlays/cloud/cluster-role.yaml"] {
        let rules = role_rules(&read(relative));
        let streamline_rules: Vec<&serde_yaml::Value> = rules
            .iter()
            .filter(|rule| {
                rule["apiGroups"].as_sequence().is_some_and(|groups| {
                    groups
                        .iter()
                        .any(|group| group.as_str() == Some("streamline.io"))
                })
            })
            .collect();
        assert_eq!(
            streamline_rules.len(),
            3,
            "{relative} must split main, status, and finalizer permissions"
        );

        let find_rule = |suffix: &str| {
            streamline_rules
                .iter()
                .copied()
                .find(|rule| {
                    let resources = rule["resources"]
                        .as_sequence()
                        .expect("Streamline RBAC rule resources");
                    match suffix {
                        "" => resources.iter().all(|resource| {
                            resource.as_str().is_some_and(|name| !name.contains('/'))
                        }),
                        _ => resources.iter().all(|resource| {
                            resource.as_str().is_some_and(|name| name.ends_with(suffix))
                        }),
                    }
                })
                .unwrap_or_else(|| panic!("{relative} is missing the `{suffix}` rule"))
        };
        let verbs = |rule: &serde_yaml::Value| {
            rule["verbs"]
                .as_sequence()
                .expect("Streamline RBAC rule verbs")
                .iter()
                .map(|verb| verb.as_str().expect("RBAC verb").to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            verbs(find_rule("")),
            vec![
                "get".to_string(),
                "list".to_string(),
                "watch".to_string(),
                "patch".to_string()
            ],
            "{relative} must not create, update, or delete user-authored Streamline CRs"
        );
        assert_eq!(
            verbs(find_rule("/status")),
            vec!["patch".to_string()],
            "{relative} status access must match patch_status usage"
        );
        assert_eq!(
            verbs(find_rule("/finalizers")),
            vec!["update".to_string()],
            "{relative} finalizer access must be limited to the finalizers subresource"
        );
    }
}

#[test]
fn cloud_fixture_validator_uses_the_real_operator_acceptance_functions() {
    let source = read("src/bin/validate-cloud-fixture.rs");
    for required in [
        "serde_json::from_str(contents)",
        "cluster.spec.validate()",
        "TopicController::unsupported_fields(&topic.spec)",
    ] {
        assert!(
            source.contains(required),
            "the cloud fixture validator must execute `{required}`"
        );
    }
}

/// The Lease lives where the operator *runs*, not where it watches, so
/// `--namespace` must not be reused for leader election.
#[test]
fn leader_election_namespace_is_independent_of_the_watched_namespace() {
    let main = read("src/main.rs");
    assert!(
        main.contains("leader_election::detect_namespace(&args.leader_election_namespace)"),
        "the Lease namespace must come from --leader-election-namespace"
    );
    assert!(
        !main.contains("detect_namespace(&args.namespace)"),
        "the watched namespace must not be reused as the Lease namespace"
    );
}

#[test]
fn cluster_wide_mode_is_documented_with_the_opt_in_overlay() {
    let readme = read("README.md");
    assert!(
        readme.contains("overlays/cloud"),
        "the README must name the opt-in cluster-wide overlay"
    );
    assert!(
        readme.contains("ClusterRole") && readme.contains("--namespace="),
        "the README must explain the overlay's RBAC and empty watch flag"
    );
}

// ---------------------------------------------------------------------------
// Troubleshooting guide: selectors and support claims must match the code
// ---------------------------------------------------------------------------

/// The label scheme the operator actually applies.
///
/// `ClusterController::common_labels` and `deploy/operator.yaml` are the source
/// of truth; a documented selector that is not one of these matches nothing,
/// and "matches nothing" is indistinguishable from "the resource was never
/// created" at a terminal. The bare `app` key is what the guide used to use.
const OPERATOR_LABEL_KEYS: &[&str] = &[
    "app.kubernetes.io/name",
    "app.kubernetes.io/instance",
    "app.kubernetes.io/managed-by",
    "app.kubernetes.io/part-of",
    "app.kubernetes.io/component",
];

/// Every `-l key=value[,key=value]` selector in a document.
fn label_selectors(markdown: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in markdown.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("-l ") {
            let tail = &rest[at + 3..];
            let selector = tail
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c| c == '`' || c == '\'' || c == '"');
            if selector.contains('=') {
                found.push((index + 1, selector.to_string()));
            }
            rest = tail;
        }
    }
    found
}

#[test]
fn the_label_keys_the_docs_may_use_are_the_ones_the_operator_sets() {
    let controller = read("src/controllers/cluster.rs");
    let deployment = read("deploy/operator.yaml");

    for key in OPERATOR_LABEL_KEYS {
        assert!(
            controller.contains(key) || deployment.contains(key),
            "{key} is not applied by the operator, so no document should select on it"
        );
    }

    // The guide's selectors are only useful if these three are really set on
    // the broker workloads; pin them at the source.
    for key in [
        "app.kubernetes.io/name",
        "app.kubernetes.io/instance",
        "app.kubernetes.io/managed-by",
    ] {
        assert!(
            controller.contains(key),
            "ClusterController must label its resources with {key}"
        );
    }
}

/// `kubectl get svc -l app=streamline` returns nothing, because nothing carries
/// an `app` label. A troubleshooting guide that prints an empty list and calls
/// it a diagnosis is worse than no guide.
#[test]
fn troubleshooting_selectors_use_labels_the_operator_applies() {
    let guide = read("docs/TROUBLESHOOTING.md");
    let selectors = label_selectors(&guide);

    assert!(
        !selectors.is_empty(),
        "the troubleshooting guide must show how to select the operator's resources"
    );

    for (line, selector) in &selectors {
        for clause in selector.split(',') {
            let key = clause.split('=').next().unwrap_or_default();
            assert!(
                OPERATOR_LABEL_KEYS.contains(&key),
                "docs/TROUBLESHOOTING.md:{line} selects on `{key}`, which the operator never \
                 sets: `{selector}` matches nothing"
            );
        }
    }
}

/// A cluster's resources are selected by name *and* instance: `name` alone
/// spans every cluster in the namespace, so a two-cluster namespace makes the
/// answer look like one cluster with twice the pods.
#[test]
fn troubleshooting_selects_a_single_cluster_by_instance() {
    let guide = read("docs/TROUBLESHOOTING.md");
    let selectors = label_selectors(&guide);

    assert!(
        selectors.iter().any(|(_, selector)| {
            selector.contains("app.kubernetes.io/name=streamline,")
                && selector.contains("app.kubernetes.io/instance=")
        }),
        "the guide must show how to scope a query to one StreamlineCluster: {selectors:#?}"
    );
    assert!(
        selectors
            .iter()
            .any(|(_, selector)| selector.contains("app.kubernetes.io/name=streamline-operator")),
        "the guide must show how to read the operator's own logs: {selectors:#?}"
    );
}

/// Proof the selector check would actually fail on the old text, rather than
/// passing because the extraction found nothing.
#[test]
fn the_selector_check_rejects_the_label_scheme_the_operator_dropped() {
    let stale = "- Verify Service is created: `kubectl get svc -l app=streamline`\n\
                 - Logs: `kubectl logs -l app=streamline-operator`\n";
    let selectors = label_selectors(stale);

    assert_eq!(selectors.len(), 2, "both stale selectors must be extracted");
    assert!(
        selectors.iter().all(|(_, selector)| {
            let key = selector.split('=').next().unwrap_or_default();
            !OPERATOR_LABEL_KEYS.contains(&key)
        }),
        "`app=` must be rejected: nothing the operator creates carries that label"
    );
}

/// Autoscaling is refused by `ClusterSpec::validate`, so there are no metrics to
/// check, no metrics-server to run, and no thresholds to review. Advice that
/// says otherwise sends a reader to debug an HPA the operator deletes.
#[test]
fn troubleshooting_does_not_promise_autoscaling_can_be_tuned() {
    let guide = read("docs/TROUBLESHOOTING.md");

    for stale in [
        "Autoscaler not scaling up",
        "Verify HPA metrics are available",
        "Check that the metrics server is running",
        "Review autoscaling thresholds",
        "metrics server is running",
    ] {
        assert!(
            !guide.contains(stale),
            "docs/TROUBLESHOOTING.md still claims `{stale}`, but autoscaling is rejected \
             before any HPA is considered"
        );
    }

    // The rejection is quoted from the validator, so a reworded refusal shows
    // up here instead of leaving the guide quoting a message nobody publishes.
    let spec: streamline_operator::ClusterSpec =
        serde_json::from_str(r#"{"autoscaling": {"enabled": true}}"#)
            .expect("an autoscaling cluster spec deserializes");
    let published = spec
        .validate()
        .expect_err("autoscaling must still be rejected")
        .into_iter()
        .find(|e| e.starts_with("autoscaling.enabled is not supported"))
        .expect("the autoscaling rejection");

    let collapsed = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed(&guide).contains(&collapsed(&published)),
        "docs/TROUBLESHOOTING.md must quote the rejection the operator publishes:\n{published}"
    );
    assert!(
        guide.contains("-hpa"),
        "the guide must name the HPA the operator deletes, so a leftover one can be found"
    );
}

/// The guide's commands are only correct inside the namespace the operator
/// watches; an unnamespaced `kubectl get` silently reads `default`.
#[test]
fn troubleshooting_commands_name_a_namespace() {
    let guide = read("docs/TROUBLESHOOTING.md");
    let mut offenders = Vec::new();

    for (index, line) in guide.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("kubectl ") {
            continue;
        }
        // Cluster-scoped reads have no namespace to name.
        if trimmed.contains("get crds") || trimmed.contains("apply -k") {
            continue;
        }
        if trimmed.contains("-n ") || trimmed.contains("--namespace") {
            continue;
        }
        offenders.push(format!("docs/TROUBLESHOOTING.md:{}: {trimmed}", index + 1));
    }

    assert!(
        offenders.is_empty(),
        "namespaced commands must name their namespace:\n  {}",
        offenders.join("\n  ")
    );
}

/// Shell commands from fenced `bash`/`sh`/`shell` blocks, with backslash
/// continuations folded into one logical command.
fn fenced_shell_commands(markdown: &str) -> Vec<(usize, String)> {
    let mut in_shell = false;
    let mut continued: Option<(usize, String)> = None;
    let mut commands = Vec::new();

    for (index, line) in markdown.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(language) = trimmed.strip_prefix("```") {
            if in_shell {
                if let Some(command) = continued.take() {
                    commands.push(command);
                }
                in_shell = false;
            } else {
                in_shell = matches!(language.trim(), "bash" | "sh" | "shell");
            }
            continue;
        }
        if !in_shell || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let carries_on = trimmed.ends_with('\\');
        let fragment = trimmed.trim_end_matches('\\').trim_end();
        if let Some((_, command)) = continued.as_mut() {
            command.push(' ');
            command.push_str(fragment);
        } else {
            continued = Some((index + 1, fragment.to_string()));
        }

        if !carries_on {
            commands.push(continued.take().expect("a command is being accumulated"));
        }
    }

    commands
}

fn kubectl_command_has_namespace(command: &str) -> bool {
    command.contains(" --all-namespaces")
        || command.contains(" -A")
        || command.contains(" -n ")
        || command.contains(" --namespace ")
        || command.contains(" --namespace=")
}

/// README troubleshooting commands operate on the default namespaced install,
/// so each namespaced command must say `-n streamline-system`. Checking only
/// fenced shell blocks avoids mistaking prose for a command and folding
/// continuations keeps `kubectl auth can-i ... \` checks accurate.
#[test]
fn readme_troubleshooting_commands_name_the_default_namespace() {
    let readme = read("README.md");
    let troubleshooting = readme
        .split_once("## Troubleshooting")
        .map(|(_, tail)| tail)
        .expect("README must contain a Troubleshooting section")
        .split_once("\n## Contributing")
        .map(|(section, _)| section)
        .expect("Troubleshooting must end before Contributing");

    let mut offenders = Vec::new();
    for (line, command) in fenced_shell_commands(troubleshooting) {
        if !command.starts_with("kubectl ") {
            continue;
        }
        if command.starts_with("kubectl get crd ") || command.starts_with("kubectl get crds ") {
            continue;
        }
        if !kubectl_command_has_namespace(&command)
            || (!command.contains(" --all-namespaces")
                && !command.contains(" -A")
                && !command.contains("-n streamline-system")
                && !command.contains("--namespace streamline-system")
                && !command.contains("--namespace=streamline-system"))
        {
            offenders.push(format!(
                "README troubleshooting block line {line}: {command}"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "README troubleshooting commands must target streamline-system:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn fenced_troubleshooting_check_rejects_an_implicit_default_namespace() {
    let example = "\
```bash
kubectl describe streamlinecluster example
kubectl auth can-i list streamlineclusters \\
  --as=system:serviceaccount:streamline-system:streamline-operator \\
  -n streamline-system
kubectl get streamlineclusters --all-namespaces
kubectl get crds | grep streamline
```
";
    let commands = fenced_shell_commands(example);
    assert_eq!(commands.len(), 4, "{commands:?}");

    let offenders: Vec<&str> = commands
        .iter()
        .map(|(_, command)| command.as_str())
        .filter(|command| {
            command.starts_with("kubectl ")
                && !command.starts_with("kubectl get crd ")
                && !command.starts_with("kubectl get crds ")
                && !kubectl_command_has_namespace(command)
        })
        .collect();
    assert_eq!(
        offenders,
        vec!["kubectl describe streamlinecluster example"]
    );
}
