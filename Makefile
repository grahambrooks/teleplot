# Developer targets. `make check` is the pre-push gate and runs exactly what CI's
# gate step runs (.github/workflows/ci.yml); `make hooks` wires it into git.
# Releases come from release.mk (release-kit): `make release` tags origin/main and
# pushes the tag. `make build-release` builds the release binary locally.
.DEFAULT_GOAL := help

.PHONY: help check pre-commit fmt fmt-check lint test hooks dev build-release smoke deny fixtures integration

CLIENT := --lib --target wasm32-unknown-unknown --no-default-features --features hydrate
SERVER := --all-targets --no-default-features --features ssr
# --locked by default, so a stale Cargo.lock fails locally exactly as it does in CI
# (CI runs `make pre-commit LOCKED=--locked`). `make check LOCKED=` to drop it.
LOCKED ?= --locked

help: ## List targets
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{ printf "  %-16s %s\n", $$1, $$2 }'

# Same checks CI runs. Push only after this prints the final line.
check: fmt-check lint test ## The pre-push gate: fmt, clippy (server and WASM client), tests
	@echo "All checks passed."

pre-commit: check ## Alias for check (what CI invokes)

fmt: ## Format the code
	cargo fmt --all

fmt-check: ## Fail if the code is not formatted
	cargo fmt --all --check

lint: ## Clippy, warnings denied, server and WASM client
	cargo clippy $(SERVER) $(LOCKED) -- -D warnings
	cargo clippy $(CLIENT) $(LOCKED) -- -D warnings

test: ## Server tests
	cargo test --no-default-features --features ssr $(LOCKED)

hooks: ## Install the prek git hooks (pre-commit and pre-push)
	prek install

dev: ## Run the dev server with live reload
	cargo leptos watch

# The client must exist before the server compiles, because the server embeds
# it. A single `cargo leptos build` compiles both at once and can miss it.
build-release: ## Build the release binary (client first, then the server)
	cargo leptos build --release --frontend-only --precompress
	cargo leptos build --release --server-only

# The release binary must serve the full UI from an empty directory.
smoke: build-release ## Check the release binary serves the full UI on its own
	./scripts/smoke.sh target/release/teleplot

deny: ## Supply-chain checks (cargo-deny)
	cargo deny check

# Rebuild Kibana fixtures against real ES + Kibana (Docker). See fixtures/README.md.
fixtures: ## Rebuild the Kibana fixtures (Docker)
	fixtures/make-fixtures.sh

# Kibana parity tests against the local fixture stacks (make fixtures, or
# `docker compose -f fixtures/docker-compose.yml --profile v8 --profile v9 up -d` and seed).
integration: ## Kibana parity tests against the local fixture stacks
	TELEPLOT_TEST_ES=8.19=http://localhost:9208,9.5=http://localhost:9209 cargo test --test elasticsearch --test mcp

include release.mk
