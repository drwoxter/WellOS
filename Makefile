.PHONY: help up down migrate seed reset server server-fixtures web fmt lint lint-fixtures test test-integration web-build check

# Synthetic fixture mode (development/test only). Every fixture-dependent
# target sets these explicitly; nothing here is inherited by `make server`.
FIXTURE_ENV := WELLOS_ENV=test WELLOS_ALLOW_SYNTHETIC_SEED=true \
	DMIND_MODEL_PROVIDER=fake WELLOS_SCRIBE_PROVIDER=fake

help:
	@grep -E '^[a-z-]+:' Makefile | sed 's/:.*//'

up: ## start local Postgres via docker compose
	docker compose -f infra/docker-compose.yml up -d

down:
	docker compose -f infra/docker-compose.yml down

migrate: ## run migrations (idempotent); WELLOS_ENV must be set (defaults to test here)
	WELLOS_ENV=$${WELLOS_ENV:-test} cargo run -p wellos-server --bin migrate

seed: ## seed SYNTHETIC data — refused outside development/test, needs dev-fixtures
	$(FIXTURE_ENV) cargo run -p wellos-server --bin seed --features dev-fixtures

reset: ## drop all data and reseed the synthetic dataset (local Postgres only)
	docker compose -f infra/docker-compose.yml exec postgres \
		psql -U wellos -d wellos -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
	$(MAKE) migrate seed

server: ## run the API server with the real configuration in .env (no fixtures compiled in)
	set -a; . ./.env; set +a; cargo run -p wellos-server

server-fixtures: ## run the API server in synthetic-fixture mode (.env.development)
	set -a; . ./.env.development; set +a; \
		cargo run -p wellos-server --features dev-fixtures

web: ## run the web UI (dev)
	cd apps/web && npm run dev

fmt:
	cargo fmt --all
	cd apps/web && npm run format

lint: ## production feature set
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cd apps/web && npm run lint

lint-fixtures: ## same, with dev-fixtures compiled in
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test: ## unit tests (no database required)
	cargo test --workspace --lib

test-integration: ## integration tests (requires Postgres from `make up`)
	$(FIXTURE_ENV) cargo test --workspace --test '*'

web-build:
	cd apps/web && npm run build

check: lint lint-fixtures test
