.PHONY: build test lint fmt clean help check integration-up integration-down test-integration generate-crds verify-crds release-manifests verify-release static

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

build: ## Build the operator
	cargo build

test: ## Run tests (hermetic — no Kubernetes or Streamline required)
	cargo test

generate-crds: ## Regenerate deploy/crds/ from the Rust CRD types
	cargo run --quiet --bin streamline-operator -- --generate-crds-dir deploy/crds

verify-crds: ## CI helper: fail if committed CRDs differ from the generator
	cargo test --locked --test crd_manifests checked_in_manifests_match_the_generator

static: ## CI helper: run the hermetic manifest/docs checks
	cargo test --locked --test static_manifests --test crd_manifests --test docs_examples

# ---------------------------------------------------------------------------
# Release
#
# deploy/operator.yaml ships a deliberately unpullable placeholder image so the
# checked-in manifests can never be mistaken for a released deployment. These
# targets are the only supported way to turn them into something runnable, and
# they refuse to guess: the caller supplies an explicit, immutable image.
# ---------------------------------------------------------------------------

IMAGE_PLACEHOLDER := ghcr.io/streamlinelabs/streamline-operator:REPLACE_WITH_RELEASED_IMAGE
CARGO_VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

release-manifests: ## Render deployable manifests: make release-manifests IMAGE=repo@sha256:...
	@if [ -z "$(IMAGE)" ]; then \
		echo "IMAGE is required, e.g."; \
		echo "  make release-manifests IMAGE=ghcr.io/streamlinelabs/streamline-operator@sha256:<digest>"; \
		exit 1; \
	fi
	@case "$(IMAGE)" in \
		*@sha256:*) ;; \
		*:*) echo "warning: $(IMAGE) is a mutable tag; prefer repo@sha256:<digest>" >&2 ;; \
		*) echo "IMAGE must include a tag or digest, got $(IMAGE)"; exit 1 ;; \
	esac
	@case "$(IMAGE)" in \
		*$(IMAGE_PLACEHOLDER)*) echo "IMAGE must not be the placeholder"; exit 1 ;; \
	esac
	@sed 's|$(IMAGE_PLACEHOLDER)|$(IMAGE)|' deploy/operator.yaml > deploy/operator.release.yaml
	@if grep -q 'REPLACE_WITH_RELEASED_IMAGE' deploy/operator.release.yaml; then \
		echo "placeholder survived substitution in deploy/operator.release.yaml"; \
		rm -f deploy/operator.release.yaml; \
		exit 1; \
	fi
	@echo "wrote deploy/operator.release.yaml using $(IMAGE)"

verify-release: ## Check tag/version/image agreement: make verify-release TAG=v1.2.3 IMAGE=...
	@if [ -z "$(TAG)" ]; then echo "TAG is required, e.g. make verify-release TAG=v$(CARGO_VERSION)"; exit 1; fi
	@if [ -z "$(IMAGE)" ]; then echo "IMAGE is required"; exit 1; fi
	@tag_version="$${TAG#v}"; \
	if [ "$$tag_version" != "$(CARGO_VERSION)" ]; then \
		echo "tag $(TAG) does not match Cargo.toml version $(CARGO_VERSION)"; exit 1; \
	fi; \
	case "$(IMAGE)" in \
		*@sha256:*) ;; \
		*":$$tag_version") ;; \
		*) echo "IMAGE $(IMAGE) is neither a digest nor the released tag $$tag_version"; exit 1 ;; \
	esac; \
	echo "release inputs agree: $(TAG) / $(CARGO_VERSION) / $(IMAGE)"

integration-up: ## Start integration services (override with STREAMLINE_TEST_IMAGE)
	docker compose -f docker-compose.test.yml up -d --wait

integration-down: ## Stop and remove integration services
	docker compose -f docker-compose.test.yml down -v

test-integration: ## Run explicitly gated integration tests (needs integration-up)
	cargo test --test integration -- --ignored --test-threads=1

lint: ## Run clippy lints
	cargo clippy --all-targets -- -D warnings

fmt: ## Format code
	cargo fmt

fmt-check: ## Check formatting
	cargo fmt --all -- --check

clean: ## Clean build artifacts
	cargo clean

check: fmt-check lint test ## Run all checks (CRD drift and static manifests are covered by cargo test)

docker: ## Build Docker image
	docker build -t streamline-operator:dev .
