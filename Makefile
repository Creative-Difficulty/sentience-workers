# Convenience targets for the local stack (see README "Local full stack").
# All of them honour the usual compose env vars (COMPOSE_PROJECT_NAME,
# COMPOSE_FILE, COMPOSE_PROFILES, DASH_CONTEXT, UNIDB_CONTEXT, ...).
COMPOSE ?= docker compose
SEED    ?= docker/seed/dev-seed.sql

.PHONY: help build up down clean logs ps seed e2e test-mock-llm

help: ## list targets
	@grep -E '^[a-z0-9-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

build: ## build all images of the default stack
	$(COMPOSE) build

up: ## start the default stack (postgres, rustfs, mock LLM, workers, dashboard) and wait until healthy
	$(COMPOSE) up -d --build --wait

down: ## stop the stack (keeps volumes)
	$(COMPOSE) down

clean: ## stop the stack and delete its volumes (database, objects)
	$(COMPOSE) down -v --remove-orphans

logs: ## follow logs of all services
	$(COMPOSE) logs -f --tail=200

ps: ## show service status
	$(COMPOSE) ps -a

seed: ## load the synthetic dev seed into the running postgres
	$(COMPOSE) exec -T postgres sh -c 'psql -X -q -v ON_ERROR_STOP=1 -U "$$POSTGRES_USER" -d "$$POSTGRES_DB"' < $(SEED)

e2e: ## run the end-to-end test (own throw-away project, torn down afterwards)
	./scripts/e2e.sh

test-mock-llm: ## unit tests of the mock LLM
	cd docker/mock-llm && python3 -m unittest -v test_mock_llm
