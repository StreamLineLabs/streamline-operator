.PHONY: build test lint fmt clean help check

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

build: ## Build the operator
	cargo build

test: ## Run tests
	cargo test

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
