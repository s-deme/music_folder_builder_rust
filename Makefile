.PHONY: image shell check build ui-check ui-build cli desktop

image:
	docker compose build dev

shell:
	docker compose run --rm dev bash

check:
	docker compose run --rm dev bash -c 'cargo fmt --check && cargo clippy -p music-folder-core -p music-folder-infra -p music-folder-cli --all-targets -- -D warnings && cargo test -p music-folder-core -p music-folder-infra -p music-folder-cli && npm --prefix ui install && npm --prefix ui run check'

build:
	docker compose run --rm dev bash -c 'npm --prefix ui install && npm --prefix ui run build && cargo build -p music-folder-cli'

ui-check:
	docker compose run --rm dev bash -c 'npm --prefix ui install && npm --prefix ui run check'

ui-build:
	docker compose run --rm dev bash -c 'npm --prefix ui install && npm --prefix ui run build'

cli:
	docker compose run --rm dev cargo build -p music-folder-cli

desktop:
	docker compose run --rm dev bash -c 'npm --prefix ui install && npm --prefix ui run build && cargo build -p music-folder-desktop'
