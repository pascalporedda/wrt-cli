use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success());
}

fn init_repo() -> TempDir {
    let td = TempDir::new().unwrap();
    git(td.path(), &["init"]);
    fs::write(
        td.path().join("package.json"),
        r#"{
  "name": "wrt-codex-e2e",
  "private": true,
  "packageManager": "pnpm@10.0.0",
  "scripts": {"dev": "echo dev"}
}
"#,
    )
    .unwrap();
    fs::write(td.path().join("README.md"), "x\n").unwrap();
    git(td.path(), &["add", "."]);
    git(
        td.path(),
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
    td
}

/// Runs the real `codex` CLI via `wrt init --print`.
///
/// This is ignored by default because it requires:
/// - network access
/// - Codex CLI installed and authenticated
/// - cost/time variability
///
/// To run:
///   RUN_CODEX_E2E=1 cargo test --test codex_e2e -- --ignored
/// Optional:
///   WRT_CODEX_E2E_MODEL=gpt-5.3-codex
#[test]
#[ignore]
fn init_print_with_real_codex_cli() {
    if std::env::var("RUN_CODEX_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping: set RUN_CODEX_E2E=1");
        return;
    }

    // Fail fast if codex isn't available.
    let has_codex = StdCommand::new("codex")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !has_codex {
        panic!("codex CLI not found in PATH");
    }

    let td = init_repo();

    let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"));
    cmd.current_dir(td.path())
        .env_remove("WRT_CODEX_MOCK_OUTPUT")
        .args(["init", "--print"]);

    if let Ok(model) = std::env::var("WRT_CODEX_E2E_MODEL") {
        let model = model.trim().to_string();
        if !model.is_empty() {
            cmd.args(["--model", &model]);
        }
    }

    let out = cmd.assert().success().get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("stdout should be valid JSON");

    // Sanity check for the minimal expected shape.
    assert!(v.get("version").is_some());
    assert!(v.get("port_block_size").is_some());
    assert!(v.get("package_manager").is_some());
    assert!(v.get("services").is_some());
    assert!(v.get("supabase").is_some());
    assert!(v.get("notes").is_some());

    // Ensure the schema-driven required key that previously caused the 400 exists.
    let pm = v
        .get("package_manager")
        .and_then(|x| x.as_object())
        .expect("package_manager object");
    assert!(pm.contains_key("notes"));
}
