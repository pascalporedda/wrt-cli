#[path = "support/compose_project.rs"]
pub mod compose_project;

use compose_project::{ComposeIsolation, ComposeProjectFixture};
use serde_json::Value;
use std::fs;
use std::process::Command;

#[test]
fn fixture_contains_the_eln_shaped_project_contract() {
    let fixture = ComposeProjectFixture::new(ComposeIsolation::Safe);
    let package: Value = serde_json::from_slice(
        &fs::read(fixture.path().join("package.json")).expect("read fixture package.json"),
    )
    .expect("parse fixture package.json");
    let compose = fs::read_to_string(fixture.compose_path()).expect("read fixture Compose file");

    assert_eq!(package["packageManager"], "pnpm@10.0.0");
    for script in [
        "setup",
        "start",
        "stop",
        "status",
        "db:migrate",
        "db:seed",
        "db:reset",
    ] {
        assert!(
            package["scripts"][script].is_string(),
            "missing {script} script"
        );
    }

    for service in ["postgres", "redis", "core-api", "web"] {
        assert!(compose.contains(&format!("  {service}:\n")));
    }
    for variable in [
        "DATABASE_URL",
        "AUTH_DATABASE_URL",
        "NOTIFICATION_DATABASE_URL",
        "REDIS_URL",
        "CORE_API_PORT",
        "VITE_API_URL",
    ] {
        assert!(compose.contains(variable), "missing {variable}");
    }

    assert_eq!(fixture.expected_ports["postgres"], 5432);
    assert_eq!(fixture.expected_ports["redis"], 6379);
    assert_eq!(fixture.expected_ports["core-api"], 3000);
    assert_eq!(fixture.expected_ports["source-http"], 8000);
    assert_eq!(fixture.expected_ports["web"], 5173);
    assert!(fixture.path().join("scripts/source-http.py").is_file());
    assert_eq!(
        fixture.command_log_path,
        fixture.path().join(".wrt-command.log")
    );
}

#[test]
fn variants_differ_only_in_the_two_isolation_hazards() {
    let safe = ComposeProjectFixture::new(ComposeIsolation::Safe);
    let blocked = ComposeProjectFixture::new(ComposeIsolation::Blocked);
    let safe_compose = fs::read_to_string(safe.compose_path()).expect("read safe Compose file");
    let blocked_compose =
        fs::read_to_string(blocked.compose_path()).expect("read blocked Compose file");

    assert!(!safe_compose.contains("container_name:"));
    assert!(safe_compose.contains("${POSTGRES_PORT:-5432}:5432"));
    assert!(blocked_compose.contains("container_name: eln-postgres"));
    assert!(blocked_compose.contains("\"5432:5432\""));

    let normalized_blocked = blocked_compose
        .replace("    container_name: eln-postgres\n", "")
        .replace("\"5432:5432\"", "\"${POSTGRES_PORT:-5432}:5432\"");
    assert_eq!(safe_compose, normalized_blocked);
}

#[test]
fn setup_script_records_the_environment_contract() {
    let fixture = ComposeProjectFixture::new(ComposeIsolation::Safe);
    let status = Command::new("sh")
        .current_dir(fixture.path())
        .args(["scripts/project-command.sh", "setup"])
        .env("COMPOSE_PROJECT_NAME", "wrt-feature")
        .env("POSTGRES_PORT", "15432")
        .env("REDIS_PORT", "16379")
        .env("CORE_API_PORT", "13000")
        .env("SOURCE_HTTP_PORT", "18000")
        .env("WEB_PORT", "15173")
        .env("DATABASE_URL", "postgresql://localhost:15432/core")
        .env("AUTH_DATABASE_URL", "postgresql://localhost:15432/auth")
        .env(
            "NOTIFICATION_DATABASE_URL",
            "postgresql://localhost:15432/notification",
        )
        .env("VITE_API_URL", "http://localhost:13000/api/v1")
        .status()
        .expect("run fixture setup command");

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(&fixture.command_log_path).expect("read fixture command log"),
        "setup\twrt-feature\t15432\t16379\t13000\t15173\tpostgresql://localhost:15432/core\tpostgresql://localhost:15432/auth\tpostgresql://localhost:15432/notification\thttp://localhost:13000/api/v1\t18000\n"
    );
}

#[test]
fn docker_compose_renders_both_variants_when_available() {
    if !docker_compose_is_available() {
        eprintln!("skipping Compose render: docker compose is unavailable");
        return;
    }

    let safe = ComposeProjectFixture::new(ComposeIsolation::Safe);
    let blocked = ComposeProjectFixture::new(ComposeIsolation::Blocked);
    let safe_config = render_compose_config(&safe, "wrt-safe");
    let blocked_config = render_compose_config(&blocked, "wrt-blocked");

    assert_eq!(published_port(&safe_config, "postgres"), "15432");
    assert_eq!(published_port(&blocked_config, "postgres"), "5432");
    assert_eq!(published_port(&safe_config, "redis"), "16379");
    assert_eq!(published_port(&safe_config, "core-api"), "13000");
    assert_eq!(published_port(&safe_config, "web"), "15173");
    assert_eq!(
        safe_config["services"]["core-api"]["environment"]["DATABASE_URL"],
        "postgresql://postgres:postgres@postgres:5432/core"
    );
    assert_eq!(
        safe_config["services"]["core-api"]["environment"]["REDIS_URL"],
        "redis://redis:6379"
    );
    assert!(
        safe_config["services"]["postgres"]
            .get("container_name")
            .is_none()
    );
    assert_eq!(
        blocked_config["services"]["postgres"]["container_name"],
        "eln-postgres"
    );
}

#[test]
fn wrt_preflight_checks_real_fixture_variants_when_available() {
    if !docker_compose_is_available() {
        eprintln!("skipping wrt Compose preflight: docker compose is unavailable");
        return;
    }

    let safe = ComposeProjectFixture::new(ComposeIsolation::Safe);
    safe.initialize_repo();
    let safe_root = tempfile::TempDir::new().expect("create safe managed root");
    let safe_output = run_root_init(&safe, safe_root.path());
    assert!(
        safe_output.status.success(),
        "safe preflight failed: {}",
        String::from_utf8_lossy(&safe_output.stderr)
    );
    assert!(safe_root.path().join("main/.wrt-command.log").is_file());

    let blocked = ComposeProjectFixture::new(ComposeIsolation::Blocked);
    blocked.initialize_repo();
    let blocked_root = tempfile::TempDir::new().expect("create blocked managed root");
    let blocked_output = run_root_init(&blocked, blocked_root.path());
    assert_eq!(blocked_output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&blocked_output.stderr);
    assert!(stderr.contains("fixed-host-port"), "{stderr}");
    assert!(stderr.contains("service=postgres"), "{stderr}");
    assert!(stderr.contains("5432"), "{stderr}");
    assert!(stderr.contains("fixed-container-name"), "{stderr}");
    assert!(stderr.contains("eln-postgres"), "{stderr}");
    assert!(!blocked_root.path().join("main/.wrt-command.log").exists());
}

fn run_root_init(fixture: &ComposeProjectFixture, root: &std::path::Path) -> std::process::Output {
    Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
        .current_dir(fixture.path())
        .args([
            "root",
            "init",
            fixture.path().to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .output()
        .expect("run wrt root init")
}

fn docker_compose_is_available() -> bool {
    Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn render_compose_config(fixture: &ComposeProjectFixture, project_name: &str) -> Value {
    let output = Command::new("docker")
        .current_dir(fixture.path())
        .args(["compose", "config", "--format", "json"])
        .env("COMPOSE_PROJECT_NAME", project_name)
        .env("WRT_NAME", project_name)
        .env("POSTGRES_PORT", "15432")
        .env("REDIS_PORT", "16379")
        .env("CORE_API_PORT", "13000")
        .env("WEB_PORT", "15173")
        .env(
            "DATABASE_URL",
            "postgresql://postgres:postgres@postgres:5432/core",
        )
        .env(
            "AUTH_DATABASE_URL",
            "postgresql://postgres:postgres@postgres:5432/auth",
        )
        .env(
            "NOTIFICATION_DATABASE_URL",
            "postgresql://postgres:postgres@postgres:5432/notification",
        )
        .env("REDIS_URL", "redis://redis:6379")
        .env("VITE_API_URL", "http://localhost:13000/api/v1")
        .output()
        .expect("run docker compose config");
    assert!(
        output.status.success(),
        "docker compose config failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse docker compose config JSON")
}

fn published_port(config: &Value, service: &str) -> String {
    let published = &config["services"][service]["ports"][0]["published"];
    published
        .as_str()
        .map(str::to_owned)
        .or_else(|| published.as_u64().map(|port| port.to_string()))
        .unwrap_or_else(|| panic!("missing published port for {service}"))
}
