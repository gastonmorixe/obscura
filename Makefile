# Obscura shortcuts for the commands that matter day to day.
# Prefer these over a bare `cargo build`: scoping to -p obscura-cli --bins
# avoids re-linking the whole workspace (V8 is the expensive part).
#
# Usage:
#   make help
#   make build-stealth
#   make test-stealth
#   make test-crate CRATE=obscura-net FEATURES=stealth
#
# Optional env overrides:
#   JOBS=4 make build-stealth
#   OPENSSL_NO_VENDOR=1 make build-stealth   # if vendored OpenSSL hits AVX-512 asm errors

.DEFAULT_GOAL := help

CARGO_INCREMENTAL ?= 0
JOBS ?= 2
OBSCURA_BIN ?= ./target/release/obscura
FEATURES_RENDER ?= render
FEATURES_STEALTH ?= render,stealth
FEATURES_STEALTH_ONLY ?= stealth

# Shared release build prefix (incremental off, limited parallelism).
CARGO_REL = CARGO_INCREMENTAL=$(CARGO_INCREMENTAL) CARGO_BUILD_JOBS=$(JOBS) cargo build --release -p obscura-cli --bins

.PHONY: help build build-render build-stealth build-stealth-norender build-norender \
	test test-stealth test-crate test-net-stealth \
	check-stealth-bin fetch-help

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "Obscura make targets\n\n"} \
		/^[a-zA-Z0-9_.-]+:.*##/ { printf "  %-24s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@printf "\nExamples:\n"
	@printf "  make build-stealth\n"
	@printf "  make test-crate CRATE=obscura-net FEATURES=stealth\n"
	@printf "  make check-stealth-bin\n"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
# Always use -p obscura-cli --bins. A bare `cargo build --features ...` at the
# workspace root can rebuild more than the CLI and burn time on V8.

build: build-render ## Alias: release CLI with render (rustls, no stealth)

build-render: ## Release CLI with render (geometry, screenshots, PDF)
	$(CARGO_REL) --features $(FEATURES_RENDER)

build-stealth: ## Release CLI with render + stealth (wreq/BoringSSL; needs cmake)
	$(CARGO_REL) --features $(FEATURES_STEALTH)

build-stealth-norender: ## Release CLI with stealth only (no default features / no render)
	$(CARGO_REL) --no-default-features --features $(FEATURES_STEALTH_ONLY)

build-norender: ## Release CLI with no default features (no render, no stealth)
	$(CARGO_REL) --no-default-features

# ---------------------------------------------------------------------------
# Test (nextest only: cargo test shares one V8 isolate and fails runtime tests)
# ---------------------------------------------------------------------------

test: ## Full release nextest with render (no-fail-fast)
	cargo nextest run --release --features $(FEATURES_RENDER) --no-fail-fast

test-stealth: ## Full release nextest with render + stealth
	cargo nextest run --release --features $(FEATURES_STEALTH) --no-fail-fast

test-crate: ## Focused nextest: make test-crate CRATE=obscura-net FEATURES=stealth
	@test -n "$(CRATE)" || (echo "Set CRATE=... (example: CRATE=obscura-net)"; exit 1)
	cargo nextest run --release --features $(or $(FEATURES),$(FEATURES_RENDER)) -p $(CRATE) --no-fail-fast

test-net-stealth: ## Shortcut: nextest obscura-net with stealth
	cargo nextest run --release --features stealth -p obscura-net --no-fail-fast

# ---------------------------------------------------------------------------
# Sanity
# ---------------------------------------------------------------------------

check-stealth-bin: ## Confirm release binary embeds Chrome/154 stealth identity
	@test -x $(OBSCURA_BIN) || (echo "Missing $(OBSCURA_BIN). Run: make build-stealth"; exit 1)
	@$(OBSCURA_BIN) --version
	@strings $(OBSCURA_BIN) | rg -m1 'Chrome/154\.0\.0\.0' \
		|| (echo "No Chrome/154 string in $(OBSCURA_BIN). Rebuild with make build-stealth."; exit 1)
	@strings $(OBSCURA_BIN) | rg -m1 'Not A\(Brand' \
		|| (echo "No stealth GREASE string in $(OBSCURA_BIN)."; exit 1)
	@echo "OK: $(OBSCURA_BIN) looks like a stealth Chrome 154 build"

fetch-help: ## Print obscura fetch --help from the release binary
	@test -x $(OBSCURA_BIN) || (echo "Missing $(OBSCURA_BIN). Run: make build-stealth"; exit 1)
	$(OBSCURA_BIN) fetch --help
