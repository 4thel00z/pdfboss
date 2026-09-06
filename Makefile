.DEFAULT_GOAL := help

# `make help` renders this file's own comments: `##@ Name` opens a section,
# `target: ## text` documents a target. Undocumented targets stay hidden.
ifdef NO_COLOR
BOLD :=
CYAN :=
DIM :=
RESET :=
else
BOLD := \033[1m
CYAN := \033[36m
DIM := \033[2m
RESET := \033[0m
endif

##@ Build

.PHONY: build
build: ## Build the whole workspace
	cargo build --workspace

.PHONY: release
release: ## Build the whole workspace with optimizations
	cargo build --workspace --release

.PHONY: clean
clean: ## Remove all build artifacts
	cargo clean

##@ Checks

.PHONY: fmt
fmt: ## Format all crates
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Fail on unformatted code, changing nothing
	cargo fmt --all -- --check

.PHONY: lint
lint: ## Clippy with warnings denied, both CI lanes
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy -p pdfboss-aio --all-targets --all-features -- -D warnings

.PHONY: test
test: ## Run the workspace tests plus the aio all-features lane
	cargo test --workspace
	cargo test -p pdfboss-aio --all-features

.PHONY: ci
ci: fmt-check lint test doc ## Everything the Rust CI runs, in order

##@ Python

.PHONY: develop
develop: ## Build and install the extension into the active venv
	maturin develop

.PHONY: test-py
test-py: develop ## Run the Python integration tests
	pytest -q

.PHONY: wheel
wheel: ## Build the release wheel with maturin
	maturin build --release

##@ Docs

.PHONY: doc
doc: ## Rustdoc for the workspace, warnings denied
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

.PHONY: book
book: ## Build the mdBook into docs/book
	mdbook build docs

.PHONY: book-serve
book-serve: ## Serve the mdBook locally and open it
	mdbook serve docs --open

##@ ISO 32000 gate

.PHONY: iso32000-index
iso32000-index: ## Rescan the sources for ISO 32000 citations into iso32000/Iso32000/Generated.lean
	cd iso32000 && lake build iso32000-index && lake exe iso32000-index .. Iso32000/Generated.lean

.PHONY: iso32000-check
iso32000-check: iso32000-index ## List every ledger row, clause or citation that would fail the gate
	cd iso32000 && lake build iso32000-report && lake exe iso32000-report --check

.PHONY: iso32000-gate
iso32000-gate: iso32000-check ## Check the ISO 32000 ledger against the source tree with Lean
	cd iso32000 && lake build

.PHONY: iso32000-report
iso32000-report: iso32000-index ## Regenerate docs/src/reference/iso32000.md from the ledger
	cd iso32000 && lake build iso32000-report && lake exe iso32000-report > ../docs/src/reference/iso32000.md

.PHONY: iso32000-vectors
iso32000-vectors: ## Regenerate the filter test vectors from the Lean reference decoders
	cd iso32000 && lake build iso32000-vectors && lake exe iso32000-vectors ../crates/pdfboss-core/tests/vectors/iso32000

##@ Benchmarks

.PHONY: bench
bench: ## Run the criterion benches (core, render, output)
	cargo bench --workspace

##@ Install

.PHONY: install
install: ## Install the pdfboss CLI to ~/.cargo/bin
	cargo install --path crates/pdfboss-cli --locked --force

##@ Help

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make $(CYAN)<target>$(RESET)\n"} \
		/^##@/ { printf "\n$(BOLD)%s$(RESET)\n", substr($$0, 5); next } \
		/^[a-zA-Z0-9_-]+:.*?##/ { printf "  $(CYAN)%-12s$(RESET) %s\n", $$1, $$2 }' \
		$(MAKEFILE_LIST)
	@printf "\n$(DIM)Docs above are generated from this Makefile's ## comments.$(RESET)\n\n"
