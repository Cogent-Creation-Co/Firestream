### Makefile ###

# A Pluripotent Makefile for Firestream.
# Designed to deploy in development, testing, and production environments.

.PHONY: help

help:
	@echo "Firestream Makefile"
	@echo ""
	@echo "Container Pattern: <container>-<action>"
	@echo "  Actions: build, start, stop, restart, clean, logs, status, credentials"
	@echo ""
	@echo "=== Containers ==="
	@echo ""
	@echo "  Databases:"
	@echo "    postgres     - PostgreSQL (use postgres-16-* or postgres-17-*)"
	@echo "    redis        - Redis (use redis-7-* or redis-8-*)"
	@echo ""
	@echo "  Data Processing:"
	@echo "    spark        - Apache Spark cluster (master + worker)"
	@echo "    kafka        - Apache Kafka streaming (KRaft mode)"
	@echo ""
	@echo "  Orchestration:"
	@echo "    airflow      - Apache Airflow workflow orchestration"
	@echo ""
	@echo "  Applications:"
	@echo "    jupyterhub   - JupyterHub notebook server"
	@echo "    odoo         - Odoo ERP system (use odoo-15-*, odoo-16-*, odoo-17-*, or odoo-18-*)"
	@echo "    superset     - Apache Superset BI dashboards"
	@echo "    supabase     - Supabase Backend-as-a-Service"
	@echo ""
	@echo "=== Examples ==="
	@echo ""
	@echo "  make airflow-start       # Start Airflow"
	@echo "  make kafka-start         # Start Kafka"
	@echo "  make postgres-17-start   # Start PostgreSQL 17"
	@echo "  make redis-7-start       # Start Redis 7"
	@echo "  make superset-start      # Start Superset"
	@echo "  make odoo-15-start       # Start Odoo 15"
	@echo "  make odoo-18-start       # Start Odoo 18 (default)"
	@echo "  make odoo-credentials    # Show Odoo login info"
	@echo ""
	@echo "=== Convenience ==="
	@echo ""
	@echo "  make containers-build-all   # Build all Nix containers"
	@echo "  make containers-status      # Status of all containers"
	@echo "  make containers-clean-all   # Clean all containers"
	@echo ""
	@echo "=== Development ==="
	@echo ""
	@echo "  make build-devcontainer  # Build devcontainer"
	@echo "  make devcontainer-start  # Start devcontainer"
	@echo "  make docker-reset        # Clean all Docker resources"
	@echo "  make nix-up              # Build Nix flake"
	@echo "  make nix-fix             # Garbage collect Nix"
	@echo "  make builder-cache-stats # Show Nix cache volume usage"
	@echo "  make builder-cache-clean # Remove Nix cache volumes"
	@echo ""
	@echo "=== SBOM / Fleet Manifest ==="
	@echo ""
	@echo "  make manifest            # Build fleet SBOM (CycloneDX + SPDX)"
	@echo "  make sbom-airflow        # Build individual container SBOM"
	@echo "  make manifest-validate   # Validate CycloneDX compliance"
	@echo "  make manifest-clean      # Clean manifest build artifacts"
	@echo ""
	@echo "=== App Lifecycle (firestream app) ==="
	@echo ""
	@echo "  make app-list                          # List known apps"
	@echo "  make app-status BACKEND=k8s            # Status of deployed apps"
	@echo "  make app-up-airflow DEMO=1             # Bring an app up (docker default)"
	@echo "  make app-up-postgresql BACKEND=k8s EPHEMERAL=1"
	@echo "  make app-down-airflow                  # Tear an app down"
	@echo "  make app-health-redis TIMEOUT=120     # Health-check an app"
	@echo "  make app-test-postgresql BACKEND=k8s  # Deploy + probe one app"
	@echo "  make app-test-all BACKEND=docker DEMO=1"
	@echo "  Vars: BACKEND(docker|k8s) TIMEOUT DEMO NO_DEMO WAIT EPHEMERAL EXTRA"
	@echo ""
	@echo "=== Documentation ==="
	@echo ""
	@echo "  make docs-dev            # Start docs dev server (port 3001)"
	@echo "  make docs-build          # Build documentation site"
	@echo "  make docs-clean          # Clean docs build artifacts"
	@echo ""
	@echo "=== Homepage ==="
	@echo ""
	@echo "  make homepage-dev        # Start homepage dev server (auto port)"
	@echo "  make homepage-build      # Build homepage site"
	@echo "  make homepage-clean      # Clean homepage build artifacts"

# ==============================================================================
# Core Variables
# ==============================================================================
BASEDIR=$(shell pwd)
PROJECT_NAME=firestream

# Devcontainer
DEVCONTAINER_COMPOSE := docker/firestream/docker-compose.devcontainer.yml
DEVCONTAINER_PREINIT := docker/firestream/docker_preinit.sh

# Documentation
DOCS_DIR := src/app/firestream-docs

# Homepage (Lakehouse TCO calculator landing)
HOMEPAGE_DIR := src/app/firestream-homepage

# Container build script (handles cross-platform Nix builds)
BUILD_CONTAINER := bin/build-container.sh

# ==============================================================================
# Generic Container Build Pattern
# ==============================================================================
container-build-%:
	@$(BUILD_CONTAINER) $*

# ==============================================================================
# Shared Dependency Targets
# ==============================================================================
build-dep-redis:
	@if ! docker image inspect firestream-redis:7 >/dev/null 2>&1; then \
		echo "Building Redis..."; \
		$(BUILD_CONTAINER) redis; \
	else \
		echo "Redis image already exists"; \
	fi

build-dep-postgresql:
	@if ! docker image inspect firestream-postgresql:17 >/dev/null 2>&1; then \
		echo "Building PostgreSQL..."; \
		$(BUILD_CONTAINER) postgresql; \
	else \
		echo "PostgreSQL image already exists"; \
	fi

# ==============================================================================
# Airflow (Nix-based container)
# ==============================================================================
AIRFLOW_DIR := src/containers/firestream/airflow
AIRFLOW_COMPOSE := $(AIRFLOW_DIR)/docker-compose.yml

airflow-build-deps: build-dep-redis build-dep-postgresql
	@echo "==> Airflow dependencies ready"

airflow-build: airflow-build-deps
	@$(BUILD_CONTAINER) airflow

airflow-start:
	@if ! docker image inspect firestream-airflow:3.0.3 >/dev/null 2>&1; then \
		echo "Airflow image not found, building..."; \
		$(MAKE) airflow-build; \
	fi
	@if ! docker image inspect firestream-redis:7 >/dev/null 2>&1; then \
		echo "Redis image not found, building..."; \
		$(BUILD_CONTAINER) redis; \
	fi
	@if ! docker image inspect firestream-postgresql:17 >/dev/null 2>&1; then \
		echo "PostgreSQL image not found, building..."; \
		$(BUILD_CONTAINER) postgresql; \
	fi
	docker compose -f $(AIRFLOW_COMPOSE) up -d
	@echo "Airflow is running at http://localhost:8090"

airflow-up: airflow-start

airflow-stop:
	docker compose -f $(AIRFLOW_COMPOSE) down

airflow-restart: airflow-stop airflow-start

airflow-logs:
	docker compose -f $(AIRFLOW_COMPOSE) logs -f

airflow-logs-%:
	docker compose -f $(AIRFLOW_COMPOSE) logs -f $*

airflow-cli:
	docker compose -f $(AIRFLOW_COMPOSE) exec airflow airflow $(CMD)

airflow-clean: airflow-stop
	docker rmi firestream-airflow:3.0.3 firestream-airflow:3.0.3-nix 2>/dev/null || true
	docker compose -f $(AIRFLOW_COMPOSE) down -v

airflow-status:
	docker compose -f $(AIRFLOW_COMPOSE) ps

airflow-credentials:
	@echo "=== Airflow Credentials ==="
	@echo "URL:      http://localhost:8090"
	@echo "Username: airflow"
	@echo "Password: airflow"
	@echo ""
	@echo "Flower:   http://localhost:5555"

# ==============================================================================
# JupyterHub (Nix-based container)
# ==============================================================================
JUPYTERHUB_DIR := src/containers/firestream/jupyterhub
JUPYTERHUB_COMPOSE := $(JUPYTERHUB_DIR)/docker-compose.yml

jupyterhub-build:
	@$(BUILD_CONTAINER) jupyterhub

jupyterhub-start:
	@if ! docker image inspect firestream-jupyterhub:5.3.0-nix >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) jupyterhub-build; \
	fi
	docker compose -f $(JUPYTERHUB_COMPOSE) up -d
	@echo "JupyterHub is running at http://localhost:8000"

jupyterhub-up: jupyterhub-start

jupyterhub-stop:
	docker compose -f $(JUPYTERHUB_COMPOSE) down

jupyterhub-restart: jupyterhub-stop jupyterhub-start

jupyterhub-logs:
	docker compose -f $(JUPYTERHUB_COMPOSE) logs -f

jupyterhub-logs-hub:
	docker compose -f $(JUPYTERHUB_COMPOSE) logs -f jupyterhub

jupyterhub-clean: jupyterhub-stop
	docker rmi firestream-jupyterhub:5.3.0-nix 2>/dev/null || true
	docker compose -f $(JUPYTERHUB_COMPOSE) down -v

jupyterhub-status:
	docker compose -f $(JUPYTERHUB_COMPOSE) ps

jupyterhub-credentials:
	@echo "=== JupyterHub Credentials ==="
	@echo "URL:      http://localhost:8000"
	@echo "Username: admin"
	@echo "Password: admin123"

# ==============================================================================
# Kafka (Nix-based container, KRaft mode)
# ==============================================================================
KAFKA_DIR := src/containers/firestream/kafka
KAFKA_COMPOSE := $(KAFKA_DIR)/docker-compose.yml
KAFKA_CLUSTER_COMPOSE := $(KAFKA_DIR)/docker-compose-cluster.yml

kafka-build:
	@$(BUILD_CONTAINER) kafka

kafka-start:
	@if ! docker image inspect firestream-kafka:4.0 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) kafka-build; \
	fi
	docker compose -f $(KAFKA_COMPOSE) up -d
	@echo "Kafka is running at localhost:9092"

kafka-up: kafka-start

kafka-stop:
	docker compose -f $(KAFKA_COMPOSE) down

kafka-restart: kafka-stop kafka-start

kafka-logs:
	docker compose -f $(KAFKA_COMPOSE) logs -f

kafka-clean: kafka-stop
	docker rmi firestream-kafka:4.0 2>/dev/null || true
	docker compose -f $(KAFKA_COMPOSE) down -v

kafka-status:
	docker compose -f $(KAFKA_COMPOSE) ps

kafka-credentials:
	@echo "=== Kafka Connection Info ==="
	@echo "Bootstrap Server: localhost:9092"
	@echo "Controller:       localhost:9093"
	@echo "Mode:             KRaft (no ZooKeeper)"
	@echo ""
	@echo "Test with:"
	@echo "  kafka-topics.sh --bootstrap-server localhost:9092 --list"
	@echo "  kafka-console-producer.sh --bootstrap-server localhost:9092 --topic test"
	@echo "  kafka-console-consumer.sh --bootstrap-server localhost:9092 --topic test --from-beginning"

kafka-cluster-start:
	@if ! docker image inspect firestream-kafka:4.0 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) kafka-build; \
	fi
	docker compose -f $(KAFKA_CLUSTER_COMPOSE) up -d
	@echo "Kafka cluster started (brokers: 9092, 9094, 9096)"

kafka-cluster-stop:
	docker compose -f $(KAFKA_CLUSTER_COMPOSE) down

kafka-cluster-logs:
	docker compose -f $(KAFKA_CLUSTER_COMPOSE) logs -f

kafka-cluster-status:
	docker compose -f $(KAFKA_CLUSTER_COMPOSE) ps

# ==============================================================================
# Odoo (Nix-based container)
# ==============================================================================
ODOO_DIR := src/containers/firestream/odoo
ODOO_COMPOSE := $(ODOO_DIR)/docker-compose.yml

odoo-build:
	@$(BUILD_CONTAINER) odoo

odoo-build-%:
	@$(BUILD_CONTAINER) odoo --version $*

# Odoo 15
odoo-15-start:
	@if ! docker image inspect firestream-odoo:15.0 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) odoo-build-15; \
	fi
	ODOO_VERSION=15 docker compose -f $(ODOO_COMPOSE) up -d
	@echo "Odoo 15 is running at http://localhost:8069"

odoo-15-stop:
	ODOO_VERSION=15 docker compose -f $(ODOO_COMPOSE) down

odoo-15-restart: odoo-15-stop odoo-15-start

odoo-15-logs:
	ODOO_VERSION=15 docker compose -f $(ODOO_COMPOSE) logs -f

odoo-15-status:
	ODOO_VERSION=15 docker compose -f $(ODOO_COMPOSE) ps

odoo-15-clean: odoo-15-stop
	docker rmi firestream-odoo:15.0 2>/dev/null || true
	ODOO_VERSION=15 docker compose -f $(ODOO_COMPOSE) down -v

odoo-15-credentials:
	@echo "=== Odoo 15 Credentials ==="
	@echo "URL:      http://localhost:8069"
	@echo "Database: firestream_odoo"
	@echo "Email:    admin"
	@echo "Password: admin"

# Odoo 16
odoo-16-start:
	@if ! docker image inspect firestream-odoo:16.0 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) odoo-build-16; \
	fi
	ODOO_VERSION=16 docker compose -f $(ODOO_COMPOSE) up -d
	@echo "Odoo 16 is running at http://localhost:8069"

odoo-16-stop:
	ODOO_VERSION=16 docker compose -f $(ODOO_COMPOSE) down

odoo-16-restart: odoo-16-stop odoo-16-start

odoo-16-logs:
	ODOO_VERSION=16 docker compose -f $(ODOO_COMPOSE) logs -f

odoo-16-status:
	ODOO_VERSION=16 docker compose -f $(ODOO_COMPOSE) ps

odoo-16-clean: odoo-16-stop
	docker rmi firestream-odoo:16.0 2>/dev/null || true
	ODOO_VERSION=16 docker compose -f $(ODOO_COMPOSE) down -v

odoo-16-credentials:
	@echo "=== Odoo 16 Credentials ==="
	@echo "URL:      http://localhost:8069"
	@echo "Database: firestream_odoo"
	@echo "Email:    admin"
	@echo "Password: admin"

# Odoo 17
odoo-17-start:
	@if ! docker image inspect firestream-odoo:17.0 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) odoo-build-17; \
	fi
	ODOO_VERSION=17 docker compose -f $(ODOO_COMPOSE) up -d
	@echo "Odoo 17 is running at http://localhost:8069"

odoo-17-stop:
	ODOO_VERSION=17 docker compose -f $(ODOO_COMPOSE) down

odoo-17-restart: odoo-17-stop odoo-17-start

odoo-17-logs:
	ODOO_VERSION=17 docker compose -f $(ODOO_COMPOSE) logs -f

odoo-17-status:
	ODOO_VERSION=17 docker compose -f $(ODOO_COMPOSE) ps

odoo-17-clean: odoo-17-stop
	docker rmi firestream-odoo:17.0 2>/dev/null || true
	ODOO_VERSION=17 docker compose -f $(ODOO_COMPOSE) down -v

odoo-17-credentials:
	@echo "=== Odoo 17 Credentials ==="
	@echo "URL:      http://localhost:8069"
	@echo "Database: firestream_odoo"
	@echo "Email:    admin"
	@echo "Password: admin"

# Odoo 18
odoo-18-start:
	@if ! docker image inspect firestream-odoo:18.0 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) odoo-build-18; \
	fi
	ODOO_VERSION=18 docker compose -f $(ODOO_COMPOSE) up -d
	@echo "Odoo 18 is running at http://localhost:8069"

odoo-18-stop:
	ODOO_VERSION=18 docker compose -f $(ODOO_COMPOSE) down

odoo-18-restart: odoo-18-stop odoo-18-start

odoo-18-logs:
	ODOO_VERSION=18 docker compose -f $(ODOO_COMPOSE) logs -f

odoo-18-status:
	ODOO_VERSION=18 docker compose -f $(ODOO_COMPOSE) ps

odoo-18-clean: odoo-18-stop
	docker rmi firestream-odoo:18.0 2>/dev/null || true
	ODOO_VERSION=18 docker compose -f $(ODOO_COMPOSE) down -v

odoo-18-credentials:
	@echo "=== Odoo 18 Credentials ==="
	@echo "URL:      http://localhost:8069"
	@echo "Database: firestream_odoo"
	@echo "Email:    admin"
	@echo "Password: admin"

# Default Odoo version aliases (18)
odoo-start: odoo-18-start
odoo-stop: odoo-18-stop
odoo-restart: odoo-18-restart
odoo-logs: odoo-18-logs
odoo-status: odoo-18-status
odoo-clean: odoo-18-clean
odoo-credentials: odoo-18-credentials
odoo-up: odoo-start

# ==============================================================================
# PostgreSQL (Nix-based container)
# ==============================================================================
POSTGRES_DIR := src/containers/firestream/postgresql
POSTGRES_COMPOSE := $(POSTGRES_DIR)/docker-compose.yml

postgres-build:
	@$(BUILD_CONTAINER) postgresql

postgres-build-%:
	@$(BUILD_CONTAINER) postgresql --version $*

# PostgreSQL 16
postgres-16-start:
	@if ! docker image inspect firestream-postgresql:16 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) postgres-build-16; \
	fi
	PG_VERSION=16 docker compose -f $(POSTGRES_COMPOSE) up -d
	@echo "PostgreSQL 16 is running at localhost:5432"

postgres-16-stop:
	PG_VERSION=16 docker compose -f $(POSTGRES_COMPOSE) down

postgres-16-restart: postgres-16-stop postgres-16-start

postgres-16-logs:
	PG_VERSION=16 docker compose -f $(POSTGRES_COMPOSE) logs -f

postgres-16-status:
	PG_VERSION=16 docker compose -f $(POSTGRES_COMPOSE) ps

postgres-16-clean: postgres-16-stop
	docker rmi firestream-postgresql:16 2>/dev/null || true
	PG_VERSION=16 docker compose -f $(POSTGRES_COMPOSE) down -v

postgres-16-credentials:
	@echo "=== PostgreSQL 16 Credentials ==="
	@echo "Host:     localhost"
	@echo "Port:     5432"
	@echo "Database: firestream"
	@echo "Username: firestream"
	@echo "Password: firestream"
	@echo ""
	@echo "Connection: psql -h localhost -U firestream -d firestream"

# PostgreSQL 17
postgres-17-start:
	@if ! docker image inspect firestream-postgresql:17 >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) postgres-build-17; \
	fi
	PG_VERSION=17 docker compose -f $(POSTGRES_COMPOSE) up -d
	@echo "PostgreSQL 17 is running at localhost:5432"

postgres-17-stop:
	PG_VERSION=17 docker compose -f $(POSTGRES_COMPOSE) down

postgres-17-restart: postgres-17-stop postgres-17-start

postgres-17-logs:
	PG_VERSION=17 docker compose -f $(POSTGRES_COMPOSE) logs -f

postgres-17-status:
	PG_VERSION=17 docker compose -f $(POSTGRES_COMPOSE) ps

postgres-17-clean: postgres-17-stop
	docker rmi firestream-postgresql:17 2>/dev/null || true
	PG_VERSION=17 docker compose -f $(POSTGRES_COMPOSE) down -v

postgres-17-credentials:
	@echo "=== PostgreSQL 17 Credentials ==="
	@echo "Host:     localhost"
	@echo "Port:     5432"
	@echo "Database: firestream"
	@echo "Username: firestream"
	@echo "Password: firestream"
	@echo ""
	@echo "Connection: psql -h localhost -U firestream -d firestream"

# Default PostgreSQL version aliases (17)
postgres-start: postgres-17-start
postgres-stop: postgres-17-stop
postgres-restart: postgres-17-restart
postgres-logs: postgres-17-logs
postgres-status: postgres-17-status
postgres-clean: postgres-17-clean
postgres-credentials: postgres-17-credentials

# ==============================================================================
# Redis (Nix-based container)
# ==============================================================================
REDIS_DIR := src/containers/firestream/redis
REDIS_COMPOSE := $(REDIS_DIR)/docker-compose.yml
REDIS_REPLICASET_COMPOSE := $(REDIS_DIR)/docker-compose-replicaset.yml

redis-build:
	@$(BUILD_CONTAINER) redis

redis-build-%:
	@$(BUILD_CONTAINER) redis --version $*

# Redis 7
redis-7-start:
	@if ! docker image inspect firestream-redis:7-nix >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) redis-build-7; \
	fi
	REDIS_VERSION=7 docker compose -f $(REDIS_COMPOSE) up -d
	@echo "Redis 7 is running at localhost:6379"

redis-7-stop:
	REDIS_VERSION=7 docker compose -f $(REDIS_COMPOSE) down

redis-7-restart: redis-7-stop redis-7-start

redis-7-logs:
	REDIS_VERSION=7 docker compose -f $(REDIS_COMPOSE) logs -f

redis-7-status:
	REDIS_VERSION=7 docker compose -f $(REDIS_COMPOSE) ps

redis-7-clean: redis-7-stop
	docker rmi firestream-redis:7-nix 2>/dev/null || true
	REDIS_VERSION=7 docker compose -f $(REDIS_COMPOSE) down -v

redis-7-credentials:
	@echo "=== Redis 7 Credentials ==="
	@echo "Host:     localhost"
	@echo "Port:     6379"
	@echo "Password: (none - ALLOW_EMPTY_PASSWORD=yes for dev)"
	@echo ""
	@echo "Connection: redis-cli -h localhost -p 6379"

# Redis 8
redis-8-start:
	@if ! docker image inspect firestream-redis:8-nix >/dev/null 2>&1; then \
		echo "Image not found, building..."; \
		$(MAKE) redis-build-8; \
	fi
	REDIS_VERSION=8 docker compose -f $(REDIS_COMPOSE) up -d
	@echo "Redis 8 is running at localhost:6379"

redis-8-stop:
	REDIS_VERSION=8 docker compose -f $(REDIS_COMPOSE) down

redis-8-restart: redis-8-stop redis-8-start

redis-8-logs:
	REDIS_VERSION=8 docker compose -f $(REDIS_COMPOSE) logs -f

redis-8-status:
	REDIS_VERSION=8 docker compose -f $(REDIS_COMPOSE) ps

redis-8-clean: redis-8-stop
	docker rmi firestream-redis:8-nix 2>/dev/null || true
	REDIS_VERSION=8 docker compose -f $(REDIS_COMPOSE) down -v

redis-8-credentials:
	@echo "=== Redis 8 Credentials ==="
	@echo "Host:     localhost"
	@echo "Port:     6379"
	@echo "Password: (none - ALLOW_EMPTY_PASSWORD=yes for dev)"
	@echo ""
	@echo "Connection: redis-cli -h localhost -p 6379"

# Default Redis version aliases (7)
redis-start: redis-7-start
redis-stop: redis-7-stop
redis-restart: redis-7-restart
redis-logs: redis-7-logs
redis-status: redis-7-status
redis-clean: redis-7-clean
redis-credentials: redis-7-credentials

# Redis replication mode
redis-replicaset-start:
	docker compose -f $(REDIS_REPLICASET_COMPOSE) up -d
	@echo "Redis replicaset started (primary: 6379, replica: 6380)"

redis-replicaset-stop:
	docker compose -f $(REDIS_REPLICASET_COMPOSE) down

redis-replicaset-logs:
	docker compose -f $(REDIS_REPLICASET_COMPOSE) logs -f

# ==============================================================================
# Spark (Nix-based container)
# ==============================================================================
SPARK_DIR := src/containers/firestream/spark
SPARK_COMPOSE := $(SPARK_DIR)/docker-compose.yml

spark-build:
	@$(BUILD_CONTAINER) spark

spark-start:
	docker compose -f $(SPARK_COMPOSE) up -d
	@echo "Spark Master UI: http://localhost:8080"

spark-up: spark-start

spark-stop:
	docker compose -f $(SPARK_COMPOSE) down

spark-restart: spark-stop spark-start

spark-logs:
	docker compose -f $(SPARK_COMPOSE) logs -f

spark-logs-master:
	docker compose -f $(SPARK_COMPOSE) logs -f spark

spark-logs-worker:
	docker compose -f $(SPARK_COMPOSE) logs -f spark-worker

spark-status:
	docker compose -f $(SPARK_COMPOSE) ps

spark-clean:
	docker compose -f $(SPARK_COMPOSE) down -v

spark-credentials:
	@echo "=== Spark Connection Info ==="
	@echo "Master UI:  http://localhost:8080"
	@echo "Master URL: spark://localhost:7077"
	@echo ""
	@echo "Submit job:"
	@echo "  spark-submit --master spark://localhost:7077 your-app.py"

# ==============================================================================
# Supabase (Official Docker Compose - no custom build)
# ==============================================================================
SUPABASE_DIR := src/containers/firestream/supabase/supabase/docker
SUPABASE_COMPOSE := $(SUPABASE_DIR)/docker-compose.yml

supabase-start:
	docker compose -f $(SUPABASE_COMPOSE) up -d
	@echo "Supabase Studio: http://localhost:3000"

supabase-up: supabase-start

supabase-stop:
	docker compose -f $(SUPABASE_COMPOSE) down

supabase-restart: supabase-stop supabase-start

supabase-logs:
	docker compose -f $(SUPABASE_COMPOSE) logs -f

supabase-status:
	docker compose -f $(SUPABASE_COMPOSE) ps

supabase-clean:
	docker compose -f $(SUPABASE_COMPOSE) down -v

supabase-credentials:
	@echo "=== Supabase Connection Info ==="
	@echo "Studio:     http://localhost:3000"
	@echo "API:        http://localhost:8000"
	@echo "Kong:       http://localhost:8000"
	@echo ""
	@echo "See .env file in $(SUPABASE_DIR) for API keys"

# ==============================================================================
# Superset (Nix-based container)
# ==============================================================================
SUPERSET_VERSION ?= 5
SUPERSET_DIR := src/containers/firestream/superset/$(SUPERSET_VERSION)
SUPERSET_COMPOSE := $(SUPERSET_DIR)/docker-compose.yml

ifeq ($(SUPERSET_VERSION),4)
  SUPERSET_IMAGE_TAG := 4.1.1
else
  SUPERSET_IMAGE_TAG := $(SUPERSET_VERSION)
endif

superset-build: superset-build-deps
	@$(BUILD_CONTAINER) superset

# Build superset dependencies (redis and postgresql)
superset-build-deps: build-dep-redis build-dep-postgresql
	@echo "==> Superset dependencies ready"

superset-start:
	@if ! docker image inspect firestream-superset:$(SUPERSET_IMAGE_TAG) >/dev/null 2>&1; then \
		echo "Superset image not found, building..."; \
		$(MAKE) superset-build; \
	fi
	@if ! docker image inspect firestream-redis:7 >/dev/null 2>&1; then \
		echo "Redis image not found, building..."; \
		$(BUILD_CONTAINER) redis; \
	fi
	@if ! docker image inspect firestream-postgresql:17 >/dev/null 2>&1; then \
		echo "PostgreSQL image not found, building..."; \
		$(BUILD_CONTAINER) postgresql; \
	fi
	docker compose -f $(SUPERSET_COMPOSE) up -d
	@echo "Superset is running at http://localhost:8088"

superset-up: superset-start

superset-stop:
	docker compose -f $(SUPERSET_COMPOSE) down

superset-restart: superset-stop superset-start

superset-logs:
	docker compose -f $(SUPERSET_COMPOSE) logs -f

superset-logs-web:
	docker compose -f $(SUPERSET_COMPOSE) logs -f superset

superset-logs-worker:
	docker compose -f $(SUPERSET_COMPOSE) logs -f superset-worker

superset-logs-flower:
	docker compose -f $(SUPERSET_COMPOSE) logs -f superset-flower

superset-status:
	docker compose -f $(SUPERSET_COMPOSE) ps

superset-clean: superset-stop
	docker rmi firestream-superset:$(SUPERSET_IMAGE_TAG) 2>/dev/null || true
	docker compose -f $(SUPERSET_COMPOSE) down -v

superset-credentials:
	@echo "=== Superset Credentials ==="
	@echo "URL:      http://localhost:8088"
	@echo "Username: admin"
	@echo "Password: admin"
	@echo ""
	@echo "Flower (task monitor): http://localhost:5555"

# ==============================================================================
# Convenience Targets
# ==============================================================================

# Build all Nix-based containers (excludes supabase which uses official compose)
# Uses batch mode for efficient serial builds with shared Nix cache
containers-build-all:
	@echo "Building all Firestream Nix containers (batch mode)..."
	@$(BUILD_CONTAINER) postgresql redis airflow kafka spark jupyterhub --version 15 odoo --version 16 odoo --version 17 odoo --version 18 odoo superset

# Clean all containers
containers-clean-all:
	@echo "Cleaning all Firestream containers..."
	-$(MAKE) airflow-clean
	-$(MAKE) jupyterhub-clean
	-$(MAKE) kafka-clean
	-$(MAKE) odoo-15-clean
	-$(MAKE) odoo-16-clean
	-$(MAKE) odoo-17-clean
	-$(MAKE) odoo-18-clean
	-$(MAKE) postgres-clean
	-$(MAKE) redis-clean
	-$(MAKE) spark-clean
	-$(MAKE) supabase-clean
	-$(MAKE) superset-clean
	@echo "All containers cleaned!"

# Status of all containers
containers-status:
	@echo "=== Airflow ===" && docker compose -f $(AIRFLOW_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== JupyterHub ===" && docker compose -f $(JUPYTERHUB_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== Kafka ===" && docker compose -f $(KAFKA_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== Odoo ===" && docker compose -f $(ODOO_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== PostgreSQL ===" && docker compose -f $(POSTGRES_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== Redis ===" && docker compose -f $(REDIS_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== Spark ===" && docker compose -f $(SPARK_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== Supabase ===" && docker compose -f $(SUPABASE_COMPOSE) ps 2>/dev/null || echo "Not running"
	@echo ""
	@echo "=== Superset ===" && docker compose -f $(SUPERSET_COMPOSE) ps 2>/dev/null || echo "Not running"

# ==============================================================================
# Devcontainer Management
# ==============================================================================

build-devcontainer:
	bash $(DEVCONTAINER_PREINIT)
	docker compose -f $(DEVCONTAINER_COMPOSE) build devcontainer

build-devcontainer-clean:
	bash $(DEVCONTAINER_PREINIT)
	docker compose -f $(DEVCONTAINER_COMPOSE) build devcontainer --no-cache

devcontainer-start:
	bash $(DEVCONTAINER_PREINIT)
	docker compose -f $(DEVCONTAINER_COMPOSE) up -d

devcontainer-stop:
	docker compose -f $(DEVCONTAINER_COMPOSE) down

# ==============================================================================
# Nix Environment
# ==============================================================================

nix-up:
	nix build .#container

nix-fix:
	nix-collect-garbage -d

# ==============================================================================
# Flake-native container builds & deploys
#
# Builds run natively against the host /nix/store when possible and fall back to
# a nixos/nix Docker builder otherwise (macOS host, cross-arch target, no nix on
# PATH, or running inside a container). Override with STRATEGY=native|docker.
#
# These drive the first-class flake apps:
#   apps.<name>-image  - build a Linux image via Docker and load it locally
#   apps.<name>-up     - build+load the stack's images, then `docker compose up -d`
#   apps.<name>-down   - `docker compose down`
#   packages.<name>-compose - the generated docker-compose.yml
#
# Examples:
#   make flake-image-airflow            # build+load firestream-airflow image
#   make flake-image-postgresql ARCH=x86_64
#   make flake-up-airflow               # deploy the full Airflow stack
#   make flake-down-airflow
#   make flake-compose-airflow          # render docker-compose.yml only
# ==============================================================================

ARCH ?=

# Build strategy: auto (default) | native | docker
# Exported so every build entry point below inherits it with no further edits:
# container-build-%, the per-app *-build targets (via BUILD_CONTAINER),
# manifest / sbom-%, and flake-image-%.
STRATEGY ?=
ifneq ($(STRATEGY),)
export FIRESTREAM_BUILD_STRATEGY := $(STRATEGY)
endif

# Build IMPLEMENTATION: bash (default) | rust
#
# Orthogonal to STRATEGY. STRATEGY picks native-vs-docker; IMPL picks WHICH
# CODE decides that and drives the build:
#
#   IMPL unset / bash  bin/build/container-images.sh + bin/build/manifest.sh
#                      (unchanged, still the default, still the proven path)
#   IMPL=rust          `firestream-ci build images` / `firestream-ci build manifest`
#
# The Rust path is a strangler under evaluation: it must be observed building a
# real image on both a Linux and a Darwin host before it can become the default
# and before the two scripts can be deleted. See the DELETION CHECKLIST at the
# top of bin/build/container-images.sh.
#
#   make redis-build IMPL=rust
#   make manifest IMPL=rust
IMPL ?=
ifneq ($(IMPL),)
export FIRESTREAM_BUILD_IMPL := $(IMPL)
endif

# Build + load a single container image (override target arch with ARCH=x86_64).
flake-image-%:
	nix run .#$*-image -- --load $(if $(ARCH),--target $(ARCH),)

# Deploy a stack: build+load its images, then docker compose up -d.
flake-up-%:
	nix run .#$*-up

# Tear a stack down.
flake-down-%:
	nix run .#$*-down

# Render the generated docker-compose.yml (no build/deploy).
flake-compose-%:
	nix build .#$*-compose -o result-$*-compose
	@echo "Compose written to: result-$*-compose/docker-compose.yml"

# ==============================================================================
# Docker Utilities
# ==============================================================================

docker-reset:
	bash bin/commands/delete.sh

# ==============================================================================
# Builder Cache Management
# ==============================================================================

# Show Nix store cache usage
builder-cache-stats:
	@echo "=== Docker fallback cache (macOS / cross-arch only) ==="
	@docker volume ls --filter name=firestream-nix-store
	@echo ""
	@for vol in $$(docker volume ls -q --filter name=firestream-nix-store); do \
		echo "  $$vol"; \
	done
	@echo ""
	@echo "=== Host Nix store (native builds) ==="
	@if command -v nix >/dev/null 2>&1; then \
		du -sh /nix/store 2>/dev/null || echo "  /nix/store (size unavailable)"; \
	else \
		echo "  nix not on PATH - all builds use the Docker fallback"; \
	fi

# Clean Nix store cache (reclaim disk space)
# This is what reclaims the Docker builder volumes (historically ~100+ GB).
# Keep firestream-nix-store-arm64 if you still build with ARCH=aarch64, and
# note that on a macOS host BOTH volumes remain live.
# Native builds instead pin store paths via GC roots under _build/; clear
# _build/ and run `nix store gc` to reclaim those.
builder-cache-clean:
	@echo "Removing Nix store cache volumes (Docker fallback builder)..."
	-docker volume rm firestream-nix-store-amd64 2>/dev/null || true
	-docker volume rm firestream-nix-store-arm64 2>/dev/null || true
	@echo "Cache cleared. Next Docker-strategy build will be slower (cold cache)."
	@echo "Note: native builds are unaffected; they use the host /nix/store."

# ==============================================================================
# SBOM / Fleet Manifest
# ==============================================================================

# Build script (handles git worktrees, Docker resources, etc.)
MANIFEST_SCRIPT := bin/build/manifest.sh

# Build fleet manifest in a Linux container (required for macOS)
# Produces: sbom-cyclonedx.json, sbom-spdx.json, manifest.json
manifest:
	@$(MANIFEST_SCRIPT)

# Build individual container SBOM in a Linux container
# Usage: make sbom-airflow, make sbom-spark, etc.
sbom-%:
	@$(MANIFEST_SCRIPT) $*

# Validate CycloneDX SBOM compliance
manifest-validate: manifest
	@echo "Validating CycloneDX SBOM..."
	@if command -v cyclonedx &>/dev/null; then \
		cyclonedx validate --input-file _build/manifest/sbom-cyclonedx.json; \
	else \
		echo "cyclonedx-cli not found. Install with: nix run nixpkgs#cyclonedx-cli"; \
		echo "Alternatively, check online at: https://cyclonedx.org/validator"; \
	fi

# Clean manifest build artifacts
manifest-clean:
	rm -rf _build/manifest _build/sbom

# ==============================================================================
# Documentation
# ==============================================================================

docs-dev:
	@echo "Starting documentation dev server..."
	@if [ ! -d "$(DOCS_DIR)/node_modules" ]; then \
		echo "Installing dependencies..."; \
		cd $(DOCS_DIR) && pnpm install; \
	fi
	cd $(DOCS_DIR) && pnpm dev

docs-build:
	@echo "Building documentation site..."
	@if [ ! -d "$(DOCS_DIR)/node_modules" ]; then \
		echo "Installing dependencies..."; \
		cd $(DOCS_DIR) && pnpm install; \
	fi
	cd $(DOCS_DIR) && pnpm build
	@echo "Documentation built successfully!"

docs-clean:
	@echo "Cleaning docs build artifacts..."
	rm -rf $(DOCS_DIR)/.next
	rm -rf $(DOCS_DIR)/out
	rm -rf $(DOCS_DIR)/.source
	@echo "Docs cleaned!"

docs-install:
	cd $(DOCS_DIR) && pnpm install

# ==============================================================================
# Rust Workspace (CI entry points)
#
# Invoked by .github/workflows/build.yaml. Pure Rust — no Nix install required
# on the runner. e2e/container builds live in later phases.
# ==============================================================================

.PHONY: test
test:
	cargo test --workspace

.PHONY: build
build:
	cargo build --workspace

# ==============================================================================
# Isolated util workspace (src/util)
#
# `src/util/` is its OWN virtual Cargo workspace (edition 2021, MSRV 1.82, its
# own Cargo.lock) holding firestream-ci, firestream-otel-cli and
# firestream-nix-build. It is deliberately NOT a member of the repo-root
# workspace: Firestream's crates are edition 2024, and src/util's lockfile
# carries ~15 duplicate transitive majors (tonic, axum x2, reqwest, rustls,
# bollard, git2, superconsole, ratatui) that would slow every root
# `cargo build --workspace` and fight Firestream's own pins.
#
# ACCEPTED CONSEQUENCE of that isolation: the root `cargo test --workspace` /
# `cargo build --workspace` above do NOT cover src/util. These two targets are
# the only way it gets built and tested.
#
# Nix equivalents (built binaries, no cargo needed):
#   nix build .#firestream-ci .#otel-cli .#firestream-nix-build
# All three are also on PATH inside `nix develop`.
#
# libz note: git2 -> libgit2-sys -> libz-sys links libz dynamically, so no test
# binary here will even launch without libz.so.1 on the loader path. The dev
# shell exports FIRESTREAM_UTIL_LIB_PATH for exactly this; if it is set we
# prepend it, otherwise we rely on the system loader path.
# ==============================================================================

UTIL_DIR := src/util

.PHONY: build-util
build-util:
	@cd $(UTIL_DIR) && \
	if [ -n "$$FIRESTREAM_UTIL_LIB_PATH" ]; then \
		export LD_LIBRARY_PATH="$$FIRESTREAM_UTIL_LIB_PATH$${LD_LIBRARY_PATH:+:$$LD_LIBRARY_PATH}"; \
	fi; \
	cargo build --workspace

.PHONY: test-util
test-util: test-strategy-parity test-registry-parity check-embedded-sync
	@cd $(UTIL_DIR) && \
	if [ -n "$$FIRESTREAM_UTIL_LIB_PATH" ]; then \
		export LD_LIBRARY_PATH="$$FIRESTREAM_UTIL_LIB_PATH$${LD_LIBRARY_PATH:+:$$LD_LIBRARY_PATH}"; \
	fi; \
	cargo test --workspace

# The anti-drift gate for the native-vs-docker predicate. bin/build/strategy.sh
# (the zero-dependency shell mirror sourced by _common.sh and docker-build.nix)
# and firestream_ci::platform (the authority) are driven over the SAME golden
# vectors in bin/build/strategy-cases.json. This target is the shell half; the
# Rust half runs as `cargo test -p firestream-ci platform` inside test-util.
.PHONY: test-strategy-parity
test-strategy-parity:
	@bash bin/build/test-strategy-parity.sh

# The anti-drift gate for the OTHER table the Rust build path has to reproduce:
# container -> Nix package name. bin/build/_common.sh's CONTAINER_REGISTRY,
# bin/nix/firestream/ci/profile.nix's `containerRegistry` and
# firestream_ci::profile::Profile::resolve_package_name are all driven from
# bin/build/registry-cases.json. This target is the shell half; the Rust half
# runs as `cargo test -p firestream-ci registry_parity` inside test-util.
#
# Why it matters: `.#redis` is redis-8 in the flake while a bare `redis` on the
# build path is redis-7. Drift there sends `make redis-7-start` into a rebuild
# loop with no error message.
.PHONY: test-registry-parity
test-registry-parity:
	@bash bin/build/test-registry-parity.sh

# The anti-drift gate for the THIRD duplicated surface: the Nix workspace that
# src/lib/rust/nix-container-builder embeds at compile time and builds images
# from at runtime. Regenerates embedded/ and diffs it against the canonical
# bin/nix/firestream + src/containers/firestream trees.
#
# Why it matters: build.rs embeds with respect_gitignore(true), so a NEW,
# untracked file under src/containers/firestream/ (say a new options helper the
# container's options.nix now imports) is silently absent from the embedded
# copy. `nix build .#odoo` succeeds from the working tree while the Rust builder
# evaluates a tree missing that import.
.PHONY: check-embedded-sync
check-embedded-sync:
	@bash bin/build/check-embedded-sync.sh

# ==============================================================================
# CI entry points
#
# `firestream-ci ci-linux` runs the phase DAG declared in the CI PROFILE
# (bin/nix/firestream/ci/profile.nix -> ci-manifest.json). There is no phase
# list here, in the makefile, or in Rust — change the profile, not these
# targets.
#
#   make ci-check   tidy + verify        (the PR gate; no images, no charts)
#   make ci         tidy + verify + build + attest   (the release pipeline)
#
# Requires the devshell: firestream-ci, nix-eval-jobs and FIRESTREAM_CI_PROFILE
# all come from `nix develop`. A bare shell is rejected by the sentinel guard
# with instructions rather than failing halfway through verify.
#
# STRATEGY passthrough (same knob as the container targets): `make ci
# STRATEGY=docker` forces the Docker builder without a code change; leaving it
# unset means `auto`, which on a Linux host with nix resolves to a native build
# against the host /nix/store. `firestream-ci platform decide` reports the
# decision and its reason.
#
# The tidy phase's store GC deliberately does NOT run on a host store — under
# native-first that store is both the CI cache and the developer's working
# store. Reclaim explicitly with `firestream-ci nix gc --allow-host`.
.PHONY: ci ci-check
ci:
	@$(if $(STRATEGY),FIRESTREAM_BUILD_STRATEGY=$(STRATEGY) ,)firestream-ci ci-linux --mode release

ci-check:
	@$(if $(STRATEGY),FIRESTREAM_BUILD_STRATEGY=$(STRATEGY) ,)firestream-ci ci-linux --mode check

# Print the phase DAG, tiers, per-phase attrs and export rules the profile
# resolves to — and fail if it would run nothing. Executes no builds.
.PHONY: ci-dry-run
ci-dry-run:
	@firestream-ci ci-linux --mode release --dry-run

# ------------------------------------------------------------------------------
# `mode=e2e` — the advisory e2e sweep (Phase 9)
#
# A THIRD ci mode alongside check/release, and the ONLY mode in which the
# `e2e-docker-*` / `e2e-k8s-*` phases exist. `make ci` and `make ci-check` are
# completely unaffected: those phases declare `modes = [ "e2e" ]`, so they are
# not merely skipped there, they are not materialised at all.
#
# What it runs: tidy -> verify (the normal required gate) -> 18 advisory,
# single-task, strictly-chained phases, each shelling out to exactly the
# `make test-e2e-*` / `make test-e2e-k8s-*` target you would run by hand.
# The chain is what serialises them; see the long comment in
# bin/nix/firestream/ci/profile.nix for why they are NOT parallel tasks.
#
# COST: a cold sweep is measured in HOURS and creates 10 real k3d clusters and
# 8 docker-compose stacks. Do not run it on a laptop you need. Every phase is
# advisory, so the worst verdict this mode can produce is PartiallyPassed
# (exit 2) — the exit code .github/workflows/*.yaml already renders as a
# non-blocking `::warning::`.
#
# STRICT: without `FIRESTREAM_E2E{,_K8S}_STRICT=1`, a host missing k3d/helm/
# docker makes every harness test SKIP, and skip is green. For an unattended
# run, set both:
#
#   FIRESTREAM_E2E_STRICT=1 FIRESTREAM_E2E_K8S_STRICT=1 make ci-e2e
#
# Both vars (and the rest of the FIRESTREAM_E2E_* contract) are in the
# profile's `passthroughVars`, so they survive a lift into the docker builder.
.PHONY: ci-e2e ci-e2e-dry-run
ci-e2e:
	@$(if $(STRATEGY),FIRESTREAM_BUILD_STRATEGY=$(STRATEGY) ,)firestream-ci ci-linux --mode e2e

ci-e2e-dry-run:
	@firestream-ci ci-linux --mode e2e --dry-run

# ==============================================================================
# E2E (Phase 1)
#
# Drives `nix run .#<stack>-up` / `-down` against canonical stacks under Docker.
# Opt-in: each test is #[ignore]'d, so plain `make test` never invokes them.
# Serialised on a process-wide Mutex AND -j1 (--test-threads=1) so output is
# readable. A cold canonical run is measured in HOURS and explicitly stays out
# of CI; the targets here are for local validation and incident reproduction.
#
# Env contract (see src/lib/rust/firestream/tests/e2e/harness.rs):
#   FIRESTREAM_E2E_STACKS=all|csv      Subset filter (default: canonical set)
#   FIRESTREAM_E2E_KEEP=1              Skip teardown (leave stack running)
#   FIRESTREAM_E2E_STRICT=1            Skip-gate failure becomes hard fail
#   FIRESTREAM_E2E_TIMEOUT_SECS=300    Per-stack readiness timeout (not build)
#   FIRESTREAM_E2E_PREBUILD=1          Pre-warm images outside the lock
#   FIRESTREAM_E2E_HTTP=0              Skip HTTP probes (TCP/protocol still run)
# ==============================================================================

.PHONY: test-e2e
test-e2e:
	cargo test -p firestream --test e2e -- --ignored --test-threads=1 --nocapture

.PHONY: test-e2e-%
test-e2e-%:
	FIRESTREAM_E2E_STACKS=$* cargo test -p firestream --test e2e -- --ignored --test-threads=1 --nocapture

# ==============================================================================
# E2E K8s harness (Phase 4)
#
# Fresh k3d cluster per chart, helm install via the bundle, pod-Ready wait
# + per-chart protocol probe. Local-only; NOT added to CI for the same reasons
# as the docker harness (cold runs are hours, flake create is multi-minute).
#
# Env contract (see src/lib/rust/firestream-e2e-k8s/src/env.rs):
#   FIRESTREAM_E2E_K8S_STACKS=all|csv      Subset filter (default: canonical 9)
#   FIRESTREAM_E2E_K8S_KEEP=1              Skip cluster + release teardown
#   FIRESTREAM_E2E_K8S_STRICT=1            Skip-gate failure becomes hard fail
#   FIRESTREAM_E2E_K8S_TIMEOUT_SECS=600    Per-chart readiness deadline
#   FIRESTREAM_E2E_K8S_PRELOAD=0           Skip Nix-built image preload
#   FIRESTREAM_E2E_K8S_CHARTS_DIR=...      Override bundle path
#   FIRESTREAM_E2E_K8S_HELM_TIMEOUT=...    Emergency override of the helm
#                                          install timeout. The chart
#                                          manifest's deployment.timeout is
#                                          authoritative; only set when a
#                                          specific chart needs headroom.
# ==============================================================================

.PHONY: test-e2e-k8s
test-e2e-k8s:
	cargo test -p firestream-e2e-k8s --test e2e_k8s -- --ignored --test-threads=1 --nocapture

.PHONY: test-e2e-k8s-%
test-e2e-k8s-%:
	FIRESTREAM_E2E_K8S_STACKS=$* \
	cargo test -p firestream-e2e-k8s --test e2e_k8s -- --ignored --test-threads=1 --nocapture

# PostgreSQL backup/restore round-trip (multi-chart: seaweedfs + postgresql).
# Explicit target (wins over the `test-e2e-k8s-%` pattern rule) so we can pin
# the cargo test name filter to just this scenario. Gated by the `pg-backup`
# filter token; deploys seaweedfs first (its post-install hook creates the
# `firestream` bucket), then postgresql with backup.enabled=true.
.PHONY: test-e2e-k8s-pg-backup
test-e2e-k8s-pg-backup:
	FIRESTREAM_E2E_K8S_STACKS=pg-backup \
	cargo test -p firestream-e2e-k8s --test e2e_k8s -- --ignored --test-threads=1 --nocapture e2e_k8s_pg_backup

# Odoo backup/restore round-trip (multi-chart: seaweedfs + odoo). Same shape
# as pg-backup: explicit target, `odoo-backup` filter token, seaweedfs first,
# then odoo with backup.enabled=true. The restore scales the Odoo Deployment
# to zero and back, so this also covers the quiesce path of the CLI.
.PHONY: test-e2e-k8s-odoo-backup
test-e2e-k8s-odoo-backup:
	FIRESTREAM_E2E_K8S_STACKS=odoo-backup \
	cargo test -p firestream-e2e-k8s --test e2e_k8s -- --ignored --test-threads=1 --nocapture e2e_k8s_odoo_backup

# ==============================================================================
# App lifecycle CLI (`firestream app ...`)
#
# Thin Makefile wrappers over the `firestream app` subcommands. Invoked via
# `cargo run -p firestream --` (the repo doesn't install a binary; same as the
# e2e targets above). The app stem (`%`) accepts a chart/app name or `all`.
#
# Subcommands: up | down | health | test | status | list
# Backends:    docker (default) | k8s
#
# Variables (override on the command line, e.g. `make app-up-airflow BACKEND=k8s`):
#   BACKEND=docker|k8s   Target backend (default: docker)
#   TIMEOUT=300          Health probe timeout, seconds (app-health-*)
#   DEMO=1               Pass --demo (seed demo data)
#   NO_DEMO=1            Pass --no-demo
#   WAIT=1               Pass --wait (block until ready) for app-up-*
#   EPHEMERAL=1          Pass --ephemeral
#   EXTRA="..."          Extra raw flags appended to app-up-*
#
# Examples:
#   make app-list
#   make app-status BACKEND=k8s
#   make app-up-airflow DEMO=1
#   make app-up-postgresql BACKEND=k8s EPHEMERAL=1
#   make app-test-postgresql BACKEND=k8s
#   make app-test-all BACKEND=docker DEMO=1
#   make app-health-redis TIMEOUT=120
#   make app-down-airflow
#   make app-up-all WAIT=1
# ==============================================================================

BACKEND ?= docker
TIMEOUT ?= 300
DEMO ?=
NO_DEMO ?=
WAIT ?=
EPHEMERAL ?=
EXTRA ?=
EPHEMERAL_FLAG = $(if $(EPHEMERAL),--ephemeral,)

.PHONY: app-list app-status app-up-all app-down-all app-test-all app-health-all

# List known apps.
app-list:
	cargo run -p firestream -- app list

# Status of deployed apps (optionally scoped to a backend).
app-status:
	cargo run -p firestream -- app status $(if $(BACKEND),--backend $(BACKEND),)

# Bring a single app up.
app-up-%:
	cargo run -p firestream -- app up $* --backend $(BACKEND) $(if $(DEMO),--demo,) $(if $(NO_DEMO),--no-demo,) $(if $(WAIT),--wait,) $(EPHEMERAL_FLAG) $(EXTRA)

# Tear a single app down.
app-down-%:
	cargo run -p firestream -- app down $* --backend $(BACKEND) $(EPHEMERAL_FLAG)

# Health-check a single app.
app-health-%:
	cargo run -p firestream -- app health $* --backend $(BACKEND) --timeout $(TIMEOUT)

# Test a single app (deploy + probe).
app-test-%:
	cargo run -p firestream -- app test $* --backend $(BACKEND) $(if $(DEMO),--demo,) $(if $(NO_DEMO),--no-demo,) --wait $(EPHEMERAL_FLAG)

# Convenience: operate on every app (`all`). Defined explicitly because the
# `%`-stem targets above won't match the bare word a user reaches for.
app-up-all:
	cargo run -p firestream -- app up all --backend $(BACKEND) $(if $(DEMO),--demo,) $(if $(NO_DEMO),--no-demo,) $(if $(WAIT),--wait,) $(EPHEMERAL_FLAG) $(EXTRA)

app-down-all:
	cargo run -p firestream -- app down all --backend $(BACKEND) $(EPHEMERAL_FLAG)

app-test-all:
	cargo run -p firestream -- app test all --backend $(BACKEND) $(if $(DEMO),--demo,) $(if $(NO_DEMO),--no-demo,) --wait $(EPHEMERAL_FLAG)

app-health-all:
	cargo run -p firestream -- app health all --backend $(BACKEND) --timeout $(TIMEOUT)
