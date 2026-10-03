.PHONY: pre-commit fmt lint test dev release smoke deny fixtures

CLIENT := --lib --target wasm32-unknown-unknown --no-default-features --features hydrate
SERVER := --all-targets --no-default-features --features ssr

# Same checks CI runs. Commit only after this prints the final line.
pre-commit: fmt lint test
	@echo "All pre-commit checks passed."

fmt:
	cargo fmt --check

lint:
	cargo clippy $(SERVER) -- -D warnings
	cargo clippy $(CLIENT) -- -D warnings

test:
	cargo test --no-default-features --features ssr

dev:
	cargo leptos watch

# The client must exist before the server compiles, because the server embeds
# it. A single `cargo leptos build` compiles both at once and can miss it.
release:
	cargo leptos build --release --frontend-only
	cargo leptos build --release --server-only

# The release binary must serve the full UI from an empty directory.
smoke: release
	./scripts/smoke.sh target/release/teleplot

deny:
	cargo deny check

# Rebuild Kibana fixtures against real ES + Kibana (Docker). See fixtures/README.md.
fixtures:
	fixtures/make-fixtures.sh
