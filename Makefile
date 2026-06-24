# dcs — common dev + release tasks. `make` or `make help` lists targets.
.DEFAULT_GOAL := help

# Native installer format(s) for `make package`, by host OS.
UNAME := $(shell uname -s)
ifeq ($(UNAME),Darwin)
  PKG_FORMATS := dmg
else ifeq ($(UNAME),Linux)
  PKG_FORMATS := deb appimage
else
  PKG_FORMATS := nsis
endif

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

.PHONY: run
run: ## Run the app (debug)
	cargo run -p dcs-ui --bin dcs

.PHONY: build
build: ## Build the optimized release binary
	cargo build --release --locked -p dcs-ui --bin dcs

.PHONY: check
check: fmt clippy test ## Full pre-commit gate: fmt + clippy + test

.PHONY: fmt
fmt: ## Format the workspace
	cargo fmt --all

.PHONY: clippy
clippy: ## Lint the workspace, warnings as errors
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: test
test: ## Test everything except the GUI crate
	cargo test --workspace --exclude dcs-ui --locked

.PHONY: icons
icons: ## Regenerate platform icons from assets/icon.svg
	./scripts/gen-icons.sh

.PHONY: package
package: build ## Build native installer(s) into ./dist (needs cargo-packager)
	@command -v cargo-packager >/dev/null || { echo "install: cargo install cargo-packager --locked"; exit 1; }
	# Absolute --out-dir: cargo-packager resolves it relative to the crate manifest,
	# not here, so without $(CURDIR) the artifacts land in crates/dcs-ui/dist.
	cargo packager --release --formats $(PKG_FORMATS) --out-dir "$(CURDIR)/dist"

.PHONY: clean
clean: ## Remove build + packaging output
	cargo clean
	rm -rf dist
