.PHONY: help up down migrate seed reset server web fmt lint test test-integration e2e check

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

reset: ## drop all data and reseed the synthetic demo dataset
	docker compose -f infra/docker-compose.yml exec postgres \
		psql -U wellos -d wellos -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
	$(MAKE) migrate seed

server: ## run the API server (loads .env)
	set -a; . ./.env; set +a; cargo run -p wellos-server

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
