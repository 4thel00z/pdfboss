.DEFAULT_GOAL := help

##@ Development

.PHONY: build
build: ## Build the whole workspace
	cargo build --workspace

.PHONY: test
test: ## Run the full workspace test suite
	cargo test --workspace

.PHONY: fmt
fmt: ## Format all crates
	cargo fmt --all

.PHONY: lint
lint: ## Clippy over all targets with warnings denied
	cargo clippy --workspace --all-targets -- -D warnings

##@ Python

.PHONY: wheel
wheel: ## Build the release wheel with maturin
	maturin build --release

.PHONY: develop
develop: ## Install the Python package into the active venv
	maturin develop

##@ Install

.PHONY: install
install: ## Install the pdfboss CLI to ~/.cargo/bin
	cargo install --path crates/pdfboss-cli --locked --force

##@ Help

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n"} /^[a-zA-Z0-9_-]+:.*?##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 } /^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) }' $(MAKEFILE_LIST)
