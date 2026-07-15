.PHONY: image shell check validate build ui-check ui-build cli desktop

image:
	docker compose build dev

shell:
	docker compose run --rm dev bash

# The canonical local validation entry point.  It must run inside the dev
# container; the WSL host intentionally need not have Rust or Node.js installed.
validate:
	docker compose run --rm dev bash -c 'npm --prefix ui ci && npm --prefix ui audit --audit-level=moderate && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && npm --prefix ui run check && npm --prefix ui run build'

check: validate

build:
	docker compose run --rm dev bash -c 'npm --prefix ui ci && npm --prefix ui run build && cargo build -p music-folder-cli'

ui-check:
	docker compose run --rm dev bash -c 'npm --prefix ui ci && npm --prefix ui run check'

ui-build:
	docker compose run --rm dev bash -c 'npm --prefix ui ci && npm --prefix ui run build'

cli:
	docker compose run --rm dev cargo build -p music-folder-cli

desktop:
	docker compose run --rm dev bash -c 'npm --prefix ui ci && npm --prefix ui run build && cargo build -p music-folder-desktop'
