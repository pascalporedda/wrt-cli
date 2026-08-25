#[path = "support/compose_project.rs"]
pub mod compose_project;

use compose_project::{ComposeIsolation, ComposeProjectFixture};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use tempfile::TempDir;

const DISCOVERY_V2: &str = r#"{
  "version": 2,
  "port_stride": 100,
  "ports": [
    {
      "key": "postgres",
      "base_port": 5432,
      "outputs": [
        {"env": "POSTGRES_PORT", "template": "{port}"},
        {"env": "DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/core"},
        {"env": "AUTH_DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/auth"},
        {"env": "NOTIFICATION_DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/notification"}
      ]
    },
    {
      "key": "redis",
      "base_port": 6379,
      "outputs": [{"env": "REDIS_PORT", "template": "{port}"}]
    },
    {
      "key": "core-api",
      "base_port": 3000,
      "outputs": [
        {"env": "CORE_API_PORT", "template": "{port}"},
        {"env": "VITE_API_URL", "template": "http://localhost:{port}/api/v1"}
      ]
    },
    {
      "key": "web",
      "base_port": 5173,
      "outputs": [{"env": "WEB_PORT", "template": "{port}"}]
    }
  ],
  "commands": {
    "setup": {"argv": ["pnpm", "setup"], "cwd": null},
    "start": {"argv": ["pnpm", "start"], "cwd": null},
    "stop": {"argv": ["pnpm", "stop"], "cwd": null},
    "status": {"argv": ["pnpm", "status"], "cwd": null},
    "db_migrate": {"argv": ["pnpm", "db:migrate"], "cwd": null},
    "db_seed": {"argv": ["pnpm", "db:seed"], "cwd": null},
    "db_reset": {"argv": ["pnpm", "db:reset"], "cwd": null}
  },
  "compose": {"files": ["compose.yaml"]},
  "supabase": null
}"#;

fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success());
}

fn init_repo() -> ComposeProjectFixture {
    let fixture = ComposeProjectFixture::new(ComposeIsolation::Safe);
    git(fixture.path(), &["init"]);
    git(fixture.path(), &["add", "."]);
    git(
        fixture.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "init",
        ],
    );
    fixture
}

fn init_managed_repo(fixture: &ComposeProjectFixture) -> TempDir {
    let managed_root = TempDir::new().unwrap();
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
        .current_dir(fixture.path())
        .args([
            "root",
            "init",
            fixture.path().to_str().unwrap(),
            "--root",
            managed_root.path().to_str().unwrap(),
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();
    managed_root
}

#[test]
fn init_print_accepts_eln_shaped_discovery_v2() {
    let fixture = init_repo();
    let managed_root = init_managed_repo(&fixture);
    let mock = fixture.path().join("discovery-v2.json");
    fs::write(&mock, DISCOVERY_V2).unwrap();

    let output = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
        .current_dir(managed_root.path().join("main"))
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init", "--print"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let config: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(config["version"], 2);
    assert_eq!(config["ports"].as_array().unwrap().len(), 4);
    assert_eq!(
        config["commands"]["setup"]["argv"],
        serde_json::json!(["pnpm", "setup"])
    );
    assert_eq!(
        config["commands"]["db_reset"]["argv"],
        serde_json::json!(["pnpm", "db:reset"])
    );
    assert_eq!(
        config["compose"]["files"],
        serde_json::json!(["compose.yaml"])
    );
    assert!(!managed_root.path().join(".wrt.json").exists());
}

#[test]
fn init_print_rejects_discovery_fields_outside_project_config_v2() {
    let fixture = init_repo();
    let managed_root = init_managed_repo(&fixture);
    let mock = fixture.path().join("invalid-discovery-v2.json");
    let invalid = DISCOVERY_V2.replace(
        "\"supabase\": null",
        "\"supabase\": null, \"notes\": \"not accepted in version 2\"",
    );
    fs::write(&mock, invalid).unwrap();

    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
        .current_dir(managed_root.path().join("main"))
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init", "--print"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "invalid project config from Codex",
        ))
        .stderr(predicates::str::contains("unknown field `notes`"));
    assert!(!managed_root.path().join(".wrt.json").exists());
}

#[test]
fn init_print_rejects_missing_discovery_v2_fields() {
    let fixture = init_repo();
    let managed_root = init_managed_repo(&fixture);
    let cases = [
        r#"{"version":2,"port_stride":100}"#,
        r#"{"version":2,"port_stride":100,"ports":[],"commands":{"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},"compose":null,"supabase":null}"#,
        r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000}],"commands":{"setup":null,"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},"compose":null,"supabase":null}"#,
        r#"{"version":2,"port_stride":100,"ports":[],"commands":{"setup":{"argv":["pnpm","setup"]},"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},"compose":null,"supabase":null}"#,
    ];

    for (index, invalid) in cases.into_iter().enumerate() {
        let mock = fixture.path().join(format!("missing-{index}.json"));
        fs::write(&mock, invalid).unwrap();
        assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
            .current_dir(managed_root.path().join("main"))
            .env("WRT_CODEX_MOCK_OUTPUT", &mock)
            .args(["init", "--print"])
            .assert()
            .code(1)
            .stderr(predicates::str::contains(
                "invalid project config from Codex",
            ));
    }
    assert!(!managed_root.path().join(".wrt.json").exists());
}

#[test]
fn init_print_rejects_declared_paths_missing_from_the_checkout() {
    let fixture = init_repo();
    let managed_root = init_managed_repo(&fixture);
    let cases = [
        DISCOVERY_V2.replace("compose.yaml", "missing-compose.yaml"),
        DISCOVERY_V2.replace(
            r#"["pnpm", "setup"], "cwd": null"#,
            r#"["pnpm", "setup"], "cwd": "missing-directory""#,
        ),
        DISCOVERY_V2.replace(
            r#""supabase": null"#,
            r#""supabase": {"config_path":"missing/supabase/config.toml"}"#,
        ),
    ];

    for (index, invalid) in cases.into_iter().enumerate() {
        let mock = fixture.path().join(format!("missing-path-{index}.json"));
        fs::write(&mock, invalid).unwrap();
        assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
            .current_dir(managed_root.path().join("main"))
            .env("WRT_CODEX_MOCK_OUTPUT", &mock)
            .args(["init", "--print"])
            .assert()
            .code(1)
            .stderr(predicates::str::contains(
                "invalid project config from Codex",
            ))
            .stderr(predicates::str::contains("does not exist"));
    }
    assert!(!managed_root.path().join(".wrt.json").exists());
}

#[test]
#[ignore]
fn init_print_with_real_codex_cli() {
    if std::env::var("RUN_CODEX_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping: set RUN_CODEX_E2E=1");
        return;
    }

    assert!(
        StdCommand::new("codex")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
        "codex CLI not found in PATH"
    );

    let fixture = init_repo();
    let managed_root = init_managed_repo(&fixture);

    let main = managed_root.path().join("main");
    let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"));
    cmd.current_dir(&main)
        .env_remove("WRT_CODEX_MOCK_OUTPUT")
        .args(["init", "--print"]);
    if let Ok(model) = std::env::var("WRT_CODEX_E2E_MODEL") {
        let model = model.trim();
        if !model.is_empty() {
            cmd.args(["--model", model]);
        }
    }

    let output = cmd.assert().success().get_output().stdout.clone();
    let config: Value = serde_json::from_slice(&output).expect("parse discovery output");

    assert_eq!(config["version"], 2);
    assert_eq!(config["port_stride"], 100);
    assert_eq!(config["supabase"], Value::Null);
    assert_eq!(
        config["compose"]["files"],
        serde_json::json!(["compose.yaml"])
    );

    for command in [
        "setup",
        "start",
        "stop",
        "status",
        "db_migrate",
        "db_seed",
        "db_reset",
    ] {
        assert_fixture_command(&config["commands"][command], command);
    }

    let ports = config["ports"].as_array().expect("ports array");
    let by_key = ports
        .iter()
        .map(|port| (port["key"].as_str().expect("port key"), port))
        .collect::<BTreeMap<_, _>>();
    for (key, base_port) in [
        ("core-api", 3000),
        ("postgres", 5432),
        ("redis", 6379),
        ("web", 5173),
    ] {
        assert_eq!(by_key[key]["base_port"], base_port);
    }

    let outputs = ports
        .iter()
        .flat_map(|port| port["outputs"].as_array().expect("outputs array"))
        .map(|output| output["env"].as_str().expect("output env"))
        .collect::<Vec<_>>();
    for env in [
        "POSTGRES_PORT",
        "REDIS_PORT",
        "CORE_API_PORT",
        "WEB_PORT",
        "DATABASE_URL",
        "AUTH_DATABASE_URL",
        "NOTIFICATION_DATABASE_URL",
        "VITE_API_URL",
    ] {
        assert!(outputs.contains(&env), "missing output {env}");
    }
}

fn assert_fixture_command(command: &Value, name: &str) {
    let argv = command["argv"]
        .as_array()
        .expect("command argv")
        .iter()
        .map(|item| item.as_str().expect("argv item"))
        .collect::<Vec<_>>();
    let script = match name {
        "db_migrate" => "db:migrate",
        "db_seed" => "db:seed",
        "db_reset" => "db:reset",
        other => other,
    };
    let package_command = argv == ["pnpm", script] || argv == ["pnpm", "run", script];
    let direct_command = argv == ["sh", "scripts/project-command.sh", script];

    assert!(
        package_command || direct_command,
        "unexpected {name} command: {argv:?}"
    );
    assert_eq!(command["cwd"], Value::Null);
}
