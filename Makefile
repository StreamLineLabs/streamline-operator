.PHONY: build test lint fmt clean help check integration-up integration-down test-integration

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

build: ## Build the operator
	cargo build

test: ## Run tests (hermetic — no Kubernetes or Streamline required)
	cargo test

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
