.PHONY: help up down migrate seed server web fmt lint test test-integration e2e check

help:
	@grep -E '^[a-z-]+:' Makefile | sed 's/:.*//'

up: ## start local Postgres via docker compose
	docker compose -f infra/docker-compose.yml up -d

down:
	docker compose -f infra/docker-compose.yml down

migrate: ## run migrations from empty database (idempotent)
	cargo run -p wellos-server --bin migrate

seed: ## seed synthetic (non-PHI) demo data
	cargo run -p wellos-server --bin seed

server: ## run the API server
	cargo run -p wellos-server

web: ## run the web UI (dev)
	cd apps/web && npm run dev

fmt:
	cargo fmt --all
	cd apps/web && npm run format

lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cd apps/web && npm run lint

test: ## unit tests (no database required)
	cargo test --workspace --lib

test-integration: ## integration tests (requires Postgres from `make up`)
	cargo test --workspace --test '*'

web-build:
	cd apps/web && npm run build

check: lint test
