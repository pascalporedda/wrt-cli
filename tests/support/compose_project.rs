use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposeIsolation {
    Safe,
    Blocked,
}

pub struct ComposeProjectFixture {
    root: TempDir,
    pub expected_ports: BTreeMap<String, u16>,
    pub command_log_path: PathBuf,
}

impl ComposeProjectFixture {
    pub fn new(isolation: ComposeIsolation) -> Self {
        let root = TempDir::new().expect("create Compose project fixture");
        let command_log_path = root.path().join(".wrt-command.log");
        let expected_ports = BTreeMap::from([
            ("core-api".to_string(), 3000),
            ("postgres".to_string(), 5432),
            ("redis".to_string(), 6379),
            ("web".to_string(), 5173),
        ]);

        fs::create_dir(root.path().join("scripts")).expect("create fixture scripts directory");
        fs::write(root.path().join("package.json"), PACKAGE_JSON)
            .expect("write fixture package.json");
        fs::write(
            root.path().join("scripts/project-command.sh"),
            PROJECT_COMMAND,
        )
        .expect("write fixture project command");
        fs::write(root.path().join("compose.yaml"), compose_yaml(isolation))
            .expect("write fixture Compose file");

        Self {
            root,
            expected_ports,
            command_log_path,
        }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    pub fn compose_path(&self) -> PathBuf {
        self.path().join("compose.yaml")
    }
}

const PACKAGE_JSON: &str = r#"{
  "name": "wrt-compose-fixture",
  "private": true,
  "packageManager": "pnpm@10.0.0",
  "scripts": {
    "setup": "sh scripts/project-command.sh setup",
    "start": "sh scripts/project-command.sh start",
    "stop": "sh scripts/project-command.sh stop",
    "status": "sh scripts/project-command.sh status",
    "db:migrate": "sh scripts/project-command.sh db:migrate",
    "db:seed": "sh scripts/project-command.sh db:seed",
    "db:reset": "sh scripts/project-command.sh db:reset"
  }
}
"#;

const PROJECT_COMMAND: &str = r#"#!/bin/sh
set -eu

log_path=${WRT_COMMAND_LOG:-.wrt-command.log}
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
  "$1" \
  "${COMPOSE_PROJECT_NAME:-}" \
  "${POSTGRES_PORT:-}" \
  "${REDIS_PORT:-}" \
  "${CORE_API_PORT:-}" \
  "${WEB_PORT:-}" \
  "${DATABASE_URL:-}" \
  "${AUTH_DATABASE_URL:-}" \
  "${NOTIFICATION_DATABASE_URL:-}" \
  "${VITE_API_URL:-}" >> "$log_path"

case "$1" in
  setup) ;;
  start) docker compose up --detach ;;
  stop) docker compose down --remove-orphans ;;
  status) docker compose ps ;;
esac
"#;

fn compose_yaml(isolation: ComposeIsolation) -> String {
    let postgres_isolation = match isolation {
        ComposeIsolation::Safe => "",
        ComposeIsolation::Blocked => "    container_name: eln-postgres\n",
    };
    let postgres_port = match isolation {
        ComposeIsolation::Safe => "${POSTGRES_PORT:-5432}:5432",
        ComposeIsolation::Blocked => "5432:5432",
    };

    format!(
        r#"services:
  postgres:
    image: postgres:16-alpine
{postgres_isolation}    environment:
      POSTGRES_PASSWORD: postgres
    ports:
      - "{postgres_port}"
    volumes:
      - postgres-data:/var/lib/postgresql/data

  redis:
    image: redis:7-alpine
    ports:
      - "${{REDIS_PORT:-6379}}:6379"

  core-api:
    image: python:3.12-alpine
    command: ["python", "-m", "http.server", "3000"]
    environment:
      DATABASE_URL: "postgresql://postgres:postgres@postgres:5432/core"
      AUTH_DATABASE_URL: "postgresql://postgres:postgres@postgres:5432/auth"
      NOTIFICATION_DATABASE_URL: "postgresql://postgres:postgres@postgres:5432/notification"
      REDIS_URL: "redis://redis:6379"
    ports:
      - "${{CORE_API_PORT:-3000}}:3000"

  web:
    image: nginx:1.27-alpine
    environment:
      VITE_API_URL: "${{VITE_API_URL:-http://localhost:3000/api/v1}}"
    ports:
      - "${{WEB_PORT:-5173}}:80"

volumes:
  postgres-data:
"#
    )
}
