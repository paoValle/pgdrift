# Two halves: the verdict (no database needed) and the measurement (needs a PostgreSQL).
#
# The demo database is a container: `make db-up` starts it, `make db-down` stops it. Nothing here
# touches a database that matters.
.DEFAULT_GOAL := help
DB_PORT ?= 55432
DB_URL ?= postgres://postgres:pgdrift@127.0.0.1:$(DB_PORT)/postgres

.PHONY: help setup db-up db-down plan prove audit report test lint fmt fmt-check ci clean

help: ## show this help
	@grep -E '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

setup: ## fetch dependencies
	cargo fetch

db-up: ## start a throwaway PostgreSQL 16 on $(DB_PORT)
	@docker rm -f pgdrift-pg > /dev/null 2>&1 || true
	@docker run -d --rm --name pgdrift-pg -p $(DB_PORT):5432 -e POSTGRES_PASSWORD=pgdrift postgres:16-alpine > /dev/null
	@printf "waiting for postgres"
	@until docker exec pgdrift-pg pg_isready -U postgres > /dev/null 2>&1; do printf "."; sleep 1; done
	@echo " ready"

db-down: ## stop it
	@docker rm -f pgdrift-pg > /dev/null 2>&1 || true

plan: ## the verdict on the risky migration (expects exit 1)
	@cargo run --quiet --release -- plan --migration examples/0001-risky.sql || true
	@echo "--- the same intent, written safely (expects exit 0) ---"
	@cargo run --quiet --release -- plan --migration examples/0002-safe.sql

prove: ## measure the risky migration against the demo database
	@cargo run --quiet --release -- prove --db-url "$(DB_URL)" --migration examples/0001-risky.sql

audit: ## what the seeded schema already hides
	@cargo run --quiet --release -- audit --db-url "$(DB_URL)"

report: ## write reports/latest.md (starts from a fresh schema)
	@cargo run --quiet --release -- report --db-url "$(DB_URL)" \
		--migration examples/0001-risky.sql --safe examples/0002-safe.sql \
		--seed examples/schema.sql --out reports/latest.md > /dev/null
	@printf "wrote reports/latest.md (%s lines)\n" "$$(wc -l < reports/latest.md)"

test: ## the lexer and the table, no database needed
	cargo test --all-targets

lint: ## clippy, warnings are errors
	cargo clippy --all-targets --all-features -- -D warnings

fmt: ## writes the files
	cargo fmt

fmt-check: ## checks formatting without writing
	cargo fmt --check

ci: fmt-check lint test ## everything that does not need a database

clean: ## removes build output and the demo report
	rm -rf target
