# vquasar — one entry point for building, checking and packaging.
#
# The targets here are the same commands CI runs, in the same order, so a green
# `make check` locally means a green pipeline. Anything CI does that this cannot
# is a trap: it turns "it passed on my machine" into a thing people say.

SHELL := /bin/bash
.DEFAULT_GOAL := help

# The version stamped into the binaries and the artifact names.
#
# `git describe` when there is a tag to describe from, so a build from a release
# tag says the tag and a build from main says how far past it is and which
# commit — a binary that cannot tell you what it is, is one nobody can support.
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo 0.0.0-unknown)
TARGET  ?= x86_64-unknown-linux-gnu
DIST    ?= dist

export VQUASAR_BUILD := $(VERSION)

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
	  | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-16s\033[0m %s\n", $$1, $$2}'

# ---- build ----------------------------------------------------------------

.PHONY: build
build: ## Debug build of the whole workspace
	cargo build --workspace

.PHONY: release
release: ## Optimised build of the whole workspace
	cargo build --workspace --release --target $(TARGET)

.PHONY: ui
ui: ## Build the web console
	cd ui && npm ci && npm run build

# ---- checks ---------------------------------------------------------------

.PHONY: check
check: fmt lint test ui-test ## Everything CI runs

.PHONY: fmt
fmt: ## Formatting (fails on a diff, like CI)
	cargo fmt --all -- --check

.PHONY: lint
lint: ## Clippy, warnings denied
	cargo clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: test
test: ## Unit + e2e tests (e2e needs PostgreSQL; see E2E_PG_ADMIN_URL)
	cargo test --workspace

.PHONY: ui-test
ui-test: ## Console type-check and component tests
	cd ui && npm ci && npx tsc --noEmit && npm run test

.PHONY: audit
audit: ## Licences, advisories and sources
	cargo deny check

# ---- packaging ------------------------------------------------------------

.PHONY: dist
dist: release ui ## Release tarballs + SHA256SUMS in ./dist
	@rm -rf $(DIST) && mkdir -p $(DIST)
	@$(MAKE) --no-print-directory pkg-control pkg-agent
	@cd $(DIST) && sha256sum *.tar.gz > SHA256SUMS
	@echo "==> $(DIST)/"
	@ls -1 $(DIST)

# A component tarball is everything needed to install it and nothing else: the
# binary, the installer, and an example config. The control tarball also
# carries the console, because a control plane without its UI is a half-install
# and the two must be the same build — a console from a different commit is the
# kind of mismatch nobody thinks to check.
.PHONY: pkg-control
pkg-control:
	$(eval D := $(DIST)/vquasar-control-$(VERSION)-$(TARGET))
	@mkdir -p $(D)/config $(D)/ui
	@cp target/$(TARGET)/release/vquasar-control $(D)/
	@cp scripts/install.sh $(D)/
	@cp config/control.toml $(D)/config/
	@cp -r ui/dist/. $(D)/ui/
	@cp LICENSE $(D)/
	@tar -C $(DIST) -czf $(D).tar.gz $(notdir $(D)) && rm -rf $(D)

.PHONY: pkg-agent
pkg-agent:
	$(eval D := $(DIST)/vquasar-agent-$(VERSION)-$(TARGET))
	@mkdir -p $(D)/config
	@cp target/$(TARGET)/release/vquasar-agent $(D)/
	@cp scripts/install.sh $(D)/
	@cp config/agent.toml $(D)/config/
	@cp LICENSE $(D)/
	@tar -C $(DIST) -czf $(D).tar.gz $(notdir $(D)) && rm -rf $(D)

# ---- local install --------------------------------------------------------

.PHONY: install-control
install-control: release ui ## Install/upgrade the control plane on this machine
	sudo scripts/install.sh control --binary target/$(TARGET)/release/vquasar-control --ui-dir ui/dist

.PHONY: install-agent
install-agent: release ## Install/upgrade the agent on this machine
	sudo scripts/install.sh agent --binary target/$(TARGET)/release/vquasar-agent

.PHONY: clean
clean: ## Remove build output
	cargo clean && rm -rf $(DIST) ui/dist
