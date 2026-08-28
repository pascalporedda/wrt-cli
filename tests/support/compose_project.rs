use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
            ("source-http".to_string(), 8000),
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
        fs::write(root.path().join("scripts/source-http.py"), SOURCE_HTTP)
            .expect("write fixture source HTTP server");
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

    pub fn initialize_repo(&self) {
        fs::write(self.path().join(".wrt.json"), WRT_CONFIG).expect("write fixture config");
        run_git(self.path(), &["init", "-b", "main"]);
        run_git(self.path(), &["add", "."]);
        run_git(
            self.path(),
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "-m",
                "fixture",
            ],
        );
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
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
  "$1" \
  "${COMPOSE_PROJECT_NAME:-}" \
  "${POSTGRES_PORT:-}" \
  "${REDIS_PORT:-}" \
  "${CORE_API_PORT:-}" \
  "${WEB_PORT:-}" \
  "${DATABASE_URL:-}" \
  "${AUTH_DATABASE_URL:-}" \
  "${NOTIFICATION_DATABASE_URL:-}" \
  "${VITE_API_URL:-}" \
  "${SOURCE_HTTP_PORT:-}" >> "$log_path"

case "$1" in
  setup) ;;
  start) docker compose up --detach ;;
  stop) docker compose down --remove-orphans ;;
  status) docker compose ps --status running --quiet | grep -q . ;;
esac
"#;

const WRT_CONFIG: &str = r#"{
  "version": 2,
  "port_stride": 100,
  "ports": [
    {"key":"postgres","base_port":5432,"outputs":[
      {"env":"POSTGRES_PORT","template":"{port}"},
      {"env":"DATABASE_URL","template":"postgresql://postgres:postgres@localhost:{port}/core"},
      {"env":"AUTH_DATABASE_URL","template":"postgresql://postgres:postgres@localhost:{port}/auth"},
      {"env":"NOTIFICATION_DATABASE_URL","template":"postgresql://postgres:postgres@localhost:{port}/notification"}
    ]},
    {"key":"redis","base_port":6379,"outputs":[
      {"env":"REDIS_PORT","template":"{port}"},
      {"env":"REDIS_URL","template":"redis://localhost:{port}"}
    ]},
    {"key":"core-api","base_port":3000,"outputs":[
      {"env":"CORE_API_PORT","template":"{port}"},
      {"env":"VITE_API_URL","template":"http://localhost:{port}/api/v1"}
    ]},
    {"key":"source-http","base_port":8000,"outputs":[
      {"env":"SOURCE_HTTP_PORT","template":"{port}"}
    ]},
    {"key":"web","base_port":5173,"outputs":[{"env":"WEB_PORT","template":"{port}"}]}
  ],
  "commands": {
    "setup":{"argv":["sh","scripts/project-command.sh","setup"],"cwd":null},
    "start":{"argv":["sh","scripts/project-command.sh","start"],"cwd":null},
    "stop":{"argv":["sh","scripts/project-command.sh","stop"],"cwd":null},
    "status":{"argv":["sh","scripts/project-command.sh","status"],"cwd":null},
    "db_migrate":{"argv":["sh","scripts/project-command.sh","db:migrate"],"cwd":null},
    "db_seed":{"argv":["sh","scripts/project-command.sh","db:seed"],"cwd":null},
    "db_reset":{"argv":["sh","scripts/project-command.sh","db:reset"],"cwd":null}
  },
  "compose":{"files":["compose.yaml"]},
  "supabase":null
}
"#;

const SOURCE_HTTP: &str = r#"import http.server
import os

port = int(os.environ.get("HTTP_PORT", os.environ.get("SOURCE_HTTP_PORT", "")))
name = os.environ["WRT_NAME"]
prefix = os.environ.get("HTTP_RESPONSE_PREFIX", "source")
bind = os.environ.get("HTTP_BIND", "127.0.0.1")

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = f"{prefix}:{name}\n".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass

http.server.ThreadingHTTPServer((bind, port), Handler).serve_forever()
"#;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success());
}

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
    command: ["python", "/app/server.py"]
    environment:
      DATABASE_URL: "postgresql://postgres:postgres@postgres:5432/core"
      AUTH_DATABASE_URL: "postgresql://postgres:postgres@postgres:5432/auth"
      NOTIFICATION_DATABASE_URL: "postgresql://postgres:postgres@postgres:5432/notification"
      REDIS_URL: "redis://redis:6379"
      WRT_NAME: "${{WRT_NAME}}"
      HTTP_BIND: "0.0.0.0"
      HTTP_PORT: "3000"
      HTTP_RESPONSE_PREFIX: "core-api"
    ports:
      - "${{CORE_API_PORT:-3000}}:3000"
    volumes:
      - ./scripts/source-http.py:/app/server.py:ro

  web:
    image: python:3.12-alpine
    command: ["python", "/app/server.py"]
    environment:
      VITE_API_URL: "${{VITE_API_URL:-http://localhost:3000/api/v1}}"
      WRT_NAME: "${{WRT_NAME}}"
      HTTP_BIND: "0.0.0.0"
      HTTP_PORT: "80"
      HTTP_RESPONSE_PREFIX: "web"
    ports:
      - "${{WEB_PORT:-5173}}:80"
    volumes:
      - ./scripts/source-http.py:/app/server.py:ro

volumes:
  postgres-data:
"#
    )
}
