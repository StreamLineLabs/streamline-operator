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

check: fmt-check lint test ## Run all checks

docker: ## Build Docker image
	docker build -t streamline-operator:dev .
