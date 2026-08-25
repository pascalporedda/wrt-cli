use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn set_origin(repo: &Path, origin: &Path) {
    git(
        repo,
        &["remote", "set-url", "origin", origin.to_str().unwrap()],
    );
}

fn set_origin_with_remote_tracking(repo: &Path, origin: &Path) {
    set_origin(repo, origin);
    git(
        repo,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
}

fn init_repo() -> TempDir {
    init_repo_on_branch("main")
}

fn init_repo_on_branch(branch: &str) -> TempDir {
    let td = TempDir::new().unwrap();
    git(td.path(), &["init", "-b", branch]);
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

fn init_managed_repo() -> (TempDir, TempDir) {
    init_managed_from(init_repo())
}

fn init_managed_from(source: TempDir) -> (TempDir, TempDir) {
    let root = TempDir::new().unwrap();
    let mut cmd = wrt_cmd();
    cmd.current_dir(source.path()).args([
        "root",
        "init",
        source.path().to_str().unwrap(),
        "--root",
        root.path().to_str().unwrap(),
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();
    (source, root)
}

fn init_bare_managed_without_main() -> (TempDir, TempDir) {
    let source = init_repo();
    let root = TempDir::new().unwrap();
    let git_dir = root.path().join(".git");
    git(
        source.path(),
        &[
            "clone",
            "--bare",
            source.path().to_str().unwrap(),
            git_dir.to_str().unwrap(),
        ],
    );
    let state_dir = git_dir.join(".wrt");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("state.json"),
        format!(
            r#"{{
  "version": 3,
  "root": {{
    "layout": "managed-root",
    "managedRoot": "{root}",
    "gitCommonDir": "{git_dir}",
    "mainWorktree": "{main}",
    "worktreesPath": "{root}",
    "createdAt": "2026-06-30T00:00:00Z"
  }},
  "allocations": {{}}
}}
"#,
            root = root.path().to_string_lossy(),
            git_dir = git_dir.to_string_lossy(),
            main = root.path().join("main").to_string_lossy(),
        ),
    )
    .unwrap();

    (source, root)
}

fn init_bare_managed_with_staging_checkout_without_main() -> (TempDir, TempDir) {
    let (source, root) = init_bare_managed_without_main();
    let git_dir = root.path().join(".git");
    let staging = root.path().join("staging");
    git(
        root.path(),
        &[
            "--git-dir",
            git_dir.to_str().unwrap(),
            "worktree",
            "add",
            staging.to_str().unwrap(),
            "main",
        ],
    );
    (source, root)
}

fn main_path(root: &TempDir) -> PathBuf {
    root.path().join("main")
}

fn worktree_path(root: &TempDir, name: &str) -> PathBuf {
    root.path().join(name)
}

fn wrt_cmd() -> assert_cmd::Command {
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
}

fn set_minimal_path(cmd: &mut assert_cmd::Command) {
    // Ensure git/sh are present but supabase is very unlikely.
    cmd.env("PATH", "/usr/bin:/bin");
}

fn complete_v2_config(input: &str) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_str(input).unwrap();
    let root = value.as_object_mut().unwrap();
    root.entry("ports").or_insert_with(|| serde_json::json!([]));
    for port in root["ports"].as_array_mut().unwrap() {
        port.as_object_mut()
            .unwrap()
            .entry("outputs")
            .or_insert_with(|| serde_json::json!([]));
    }
    let commands = root
        .entry("commands")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .unwrap();
    for name in [
        "setup",
        "start",
        "stop",
        "status",
        "db_migrate",
        "db_seed",
        "db_reset",
    ] {
        let command = commands.entry(name).or_insert(serde_json::Value::Null);
        if let Some(command) = command.as_object_mut() {
            command.entry("cwd").or_insert(serde_json::Value::Null);
        }
    }
    root.entry("compose").or_insert(serde_json::Value::Null);
    root.entry("supabase").or_insert(serde_json::Value::Null);
    serde_json::to_vec(&value).unwrap()
}

#[cfg(unix)]
fn write_mock_bin(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[cfg(unix)]
fn write_mock_supabase(dir: &Path) {
    write_mock_bin(
        dir,
        "supabase",
        r#"#!/bin/sh
printf 'supabase %s [%s] branch=%s\n' "$*" "$PWD" "$GITHUB_HEAD_REF" >> "$MOCK_LOG"
case "$1" in
  status)
    test -f .mock_supabase_started || exit 1
    printf '{"API_URL":"http://127.0.0.1:54321","DB_URL":"postgresql://postgres:postgres@127.0.0.1:54322/postgres","ANON_KEY":"anon","SERVICE_ROLE_KEY":"service","JWT_SECRET":"jwt","STUDIO_URL":"http://127.0.0.1:54323"}\n'
    ;;
  start)
    if [ -n "${EXPECT_WRT_ENV:-}" ]; then test -f "$EXPECT_WRT_ENV" || exit 31; fi
    touch .mock_supabase_started
    ;;
  stop) rm -f .mock_supabase_started ;;
esac
"#,
    );
}

#[cfg(unix)]
fn write_mock_docker(dir: &Path) {
    write_mock_bin(
        dir,
        "docker",
        r#"#!/bin/sh
set -eu
if [ "${MOCK_DOCKER_MODE:-safe}" = fail ]; then
  printf 'mock render failed\n' >&2
  exit 19
fi
if [ "${MOCK_DOCKER_MODE:-safe}" = blocked ]; then
  printf '{"services":{"postgres":{"container_name":"eln-postgres","ports":[{"target":5432,"published":"5432","protocol":"tcp"}]},"redis":{"ports":[{"target":6379,"published":"%s","protocol":"tcp"}]}}}\n' "$REDIS_PORT"
  exit 0
fi
printf '{"services":{"redis":{"ports":[{"protocol":"tcp","published":"%s","target":6379}]},"postgres":{"ports":[{"protocol":"tcp","published":"%s","target":5432}]}}}\n' "$REDIS_PORT" "$POSTGRES_PORT"
"#,
    );
}

#[cfg(unix)]
fn compose_source() -> TempDir {
    let source = init_repo();
    fs::write(
        source.path().join(".wrt.json"),
        complete_v2_config(
            r#"{
  "version":2,
  "port_stride":100,
  "ports":[
    {"key":"postgres","base_port":5432,"outputs":[{"env":"POSTGRES_PORT","template":"{port}"}]},
    {"key":"redis","base_port":6379,"outputs":[{"env":"REDIS_PORT","template":"{port}"}]}
  ],
  "commands":{"setup":{"argv":["sh","setup.sh"]}},
  "compose":{"files":["compose.yaml"]}
}"#,
        ),
    )
    .unwrap();
    fs::write(
        source.path().join("compose.yaml"),
        "services:\n  postgres:\n    image: postgres\n    ports:\n      - \"${POSTGRES_PORT}:5432\"\n  redis:\n    image: redis\n    ports:\n      - \"${REDIS_PORT}:6379\"\n",
    )
    .unwrap();
    fs::write(
        source.path().join("setup.sh"),
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$WRT_NAME\" >> \"$WRT_WORKTREE_PATH/setup.log\"\n",
    )
    .unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add compose project",
        ],
    );
    source
}

#[cfg(unix)]
fn init_compose_managed(source: &TempDir, docker_bin: &Path) -> TempDir {
    let root = TempDir::new().unwrap();
    wrt_cmd()
        .current_dir(source.path())
        .env("PATH", format!("{}:/usr/bin:/bin", docker_bin.display()))
        .args([
            "root",
            "init",
            source.path().to_str().unwrap(),
            "--root",
            root.path().to_str().unwrap(),
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();
    root
}

#[test]
fn help_works_outside_git_repo() {
    let td = TempDir::new().unwrap();

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).arg("help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn commands_require_managed_root() {
    let td = init_repo();

    wrt_cmd()
        .current_dir(td.path())
        .arg("ls")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a wrt managed root"));
}

#[cfg(unix)]
#[test]
fn doctor_reports_safe_and_blocked_compose_renders() {
    let source = compose_source();
    let bin = TempDir::new().unwrap();
    write_mock_docker(bin.path());
    let managed = init_compose_managed(&source, bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    let state_path = managed.path().join(".git/.wrt/state.json");
    let state_before = fs::read(&state_path).unwrap();

    wrt_cmd()
        .current_dir(main_path(&managed))
        .env("PATH", &path)
        .arg("doctor")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "main: Compose isolation check passed",
        ));

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_DOCKER_MODE", "blocked")
        .args(["doctor", "main"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("fixed-host-port"))
        .stderr(predicate::str::contains("service=postgres"))
        .stderr(predicate::str::contains("5432"))
        .stderr(predicate::str::contains("fixed-container-name"))
        .stderr(predicate::str::contains("eln-postgres"));

    assert_eq!(fs::read(state_path).unwrap(), state_before);
}

#[cfg(unix)]
#[test]
fn doctor_render_failure_and_automatic_setup_are_blocking() {
    let source = compose_source();
    let bin = TempDir::new().unwrap();
    write_mock_docker(bin.path());
    let managed = init_compose_managed(&source, bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_DOCKER_MODE", "fail")
        .args(["doctor", "main"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("render-failed"))
        .stderr(predicate::str::contains("exit status: 19"))
        .stderr(predicate::str::contains("mock render failed").not());

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_DOCKER_MODE", "blocked")
        .args([
            "new",
            "blocked-feature",
            "--install",
            "false",
            "--supabase",
            "none",
            "--db",
            "false",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "Compose isolation preflight failed",
        ));

    assert!(
        !worktree_path(&managed, "blocked-feature")
            .join("setup.log")
            .exists()
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(managed.path().join(".git/.wrt/state.json")).unwrap())
            .unwrap();
    assert_eq!(state["allocations"]["blocked-feature"]["status"], "failed");
}

#[cfg(unix)]
#[test]
fn compose_config_without_project_setup_keeps_the_legacy_path_out_of_preflight() {
    let source = init_repo();
    fs::write(
        source.path().join(".wrt.json"),
        complete_v2_config(
            r#"{
  "version":2,
  "port_stride":100,
  "commands":{"setup":null},
  "compose":{"files":["compose.yaml"]}
}"#,
        ),
    )
    .unwrap();
    fs::write(
        source.path().join("compose.yaml"),
        "services:\n  api:\n    image: busybox\n    ports:\n      - \"9000:80\"\n",
    )
    .unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add compose without project setup",
        ],
    );
    let bin = TempDir::new().unwrap();
    let docker_log = bin.path().join("docker.log");
    write_mock_bin(
        bin.path(),
        "docker",
        "#!/bin/sh\nprintf 'called\\n' >> \"$MOCK_LOG\"\nexit 99\n",
    );
    let managed = TempDir::new().unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    wrt_cmd()
        .current_dir(source.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &docker_log)
        .args([
            "root",
            "init",
            source.path().to_str().unwrap(),
            "--root",
            managed.path().to_str().unwrap(),
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &docker_log)
        .args([
            "new",
            "legacy-compose",
            "--install",
            "false",
            "--supabase",
            "none",
            "--db",
            "false",
        ])
        .assert()
        .success();

    assert!(!docker_log.exists());
}

#[test]
fn ls_empty() {
    let (_source, td) = init_managed_repo();

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).arg("ls");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("main"))
        .stdout(predicate::str::contains("block=0"));

    let exclude = td.path().join(".git").join("info").join("exclude");
    let ex = fs::read_to_string(exclude).unwrap();
    assert!(!ex.lines().any(|l| l.trim() == ".worktrees/"));
    assert!(ex.lines().any(|l| l.trim() == ".wrt.env"));
    assert!(ex.lines().any(|l| l.trim() == ".wrt.json"));
}

#[test]
fn init_print_uses_mock_output() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);

    let mock = td.path().join("mock.json");
    fs::write(
        &mock,
        r#"{"version":1,"port_block_size":100,"package_manager":{"name":"unknown","install_command":["npm","install"]},"services":[],"supabase":{"detected":false}}"#,
    )
    .unwrap();

    let mut cmd = wrt_cmd();
    cmd.current_dir(&main)
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init", "--print"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("\"version\": 1"));

    assert!(!main.join(".wrt.json").exists());
    assert!(!td.path().join(".wrt.json").exists());
}

#[test]
fn init_writes_config_and_respects_force() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);

    let mock = td.path().join("mock.json");
    fs::write(
        &mock,
        r#"{"version":1,"port_block_size":100,"package_manager":{"name":"unknown","install_command":["npm","install"]},"services":[],"supabase":{"detected":false}}"#,
    )
    .unwrap();

    wrt_cmd()
        .current_dir(&main)
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init"])
        .assert()
        .success();

    let out_path = td.path().join(".wrt.json");
    assert!(out_path.exists());
    assert!(!main.join(".wrt.json").exists());
    let s = fs::read_to_string(&out_path).unwrap();
    assert!(s.contains("\"version\": 1"));
    assert!(s.ends_with('\n'));

    // Without --force, should refuse overwrite.
    wrt_cmd()
        .current_dir(&main)
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists"));

    // With --force, should overwrite.
    wrt_cmd()
        .current_dir(&main)
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init", "--force"])
        .assert()
        .success();
}

#[test]
fn init_rejects_invalid_project_config_without_writing() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    let mock = td.path().join("invalid-mock.json");
    fs::write(
        &mock,
        complete_v2_config(r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":[]}}}"#),
    )
    .unwrap();

    wrt_cmd()
        .current_dir(&main)
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "invalid project config from Codex",
        ))
        .stderr(predicate::str::contains("setup argv must not be empty"));

    assert!(!td.path().join(".wrt.json").exists());
}

#[test]
fn invalid_checkout_config_does_not_fall_back_for_env() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    fs::write(
        td.path().join(".wrt.json"),
        r#"{"version":1,"port_block_size":100}"#,
    )
    .unwrap();
    fs::write(main.join(".wrt.json"), "not json").unwrap();

    wrt_cmd()
        .current_dir(&main)
        .args(["env", "main"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("invalid"))
        .stderr(predicate::str::contains(".wrt.json"));
}

#[test]
fn env_reads_managed_root_config_when_checkout_has_none() {
    let source = init_repo();
    fs::write(
        source.path().join(".wrt.json"),
        complete_v2_config(
            r#"{
  "version": 2,
  "port_stride": 100,
  "ports": [
    {"key":"postgres","base_port":5432,"outputs":[{"env":"DATABASE_URL","template":"postgresql://postgres:postgres@localhost:{port}/eln"}]},
    {"key":"redis","base_port":6379,"outputs":[{"env":"REDIS_URL","template":"redis://localhost:{port}"}]},
    {"key":"rabbitmq","base_port":5672,"outputs":[{"env":"AMQP_URL","template":"amqp://localhost:{port}"}]},
    {"key":"core-api","base_port":3000,"outputs":[{"env":"CORE_API_PORT","template":"{port}"},{"env":"CORE_API_URL","template":"http://localhost:{port}/api/v1"}]},
    {"key":"oauth","base_port":8181,"outputs":[{"env":"OAUTH_CALLBACK_URL","template":"http://localhost:{port}/callback"}]}
  ]
}
"#,
        ),
    )
    .unwrap();
    git(source.path(), &["add", ".wrt.json"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add wrt config",
        ],
    );
    let (_source, td) = init_managed_from(source);
    let main = main_path(&td);
    fs::remove_file(main.join(".wrt.json")).unwrap();

    wrt_cmd()
        .current_dir(&main)
        .args(["env", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "export DATABASE_URL='postgresql://postgres:postgres@localhost:5432/eln'",
        ))
        .stdout(predicate::str::contains(
            "export REDIS_URL='redis://localhost:6379'",
        ))
        .stdout(predicate::str::contains(
            "export AMQP_URL='amqp://localhost:5672'",
        ))
        .stdout(predicate::str::contains("export CORE_API_PORT='3000'"))
        .stdout(predicate::str::contains(
            "export CORE_API_URL='http://localhost:3000/api/v1'",
        ))
        .stdout(predicate::str::contains(
            "export OAUTH_CALLBACK_URL='http://localhost:8181/callback'",
        ))
        .stdout(predicate::str::contains("export WRT_NAME='main'"));
}

#[cfg(unix)]
#[test]
fn new_patches_supabase_and_sets_skip_worktree_when_isolated() {
    let source = init_repo();

    let sbdir = source.path().join("supabase");
    fs::create_dir_all(&sbdir).unwrap();
    fs::write(
        sbdir.join("config.toml"),
        "project_id = \"myproj\"\nport = 5432\nauth_site_url = \"http://localhost:3000\"\n",
    )
    .unwrap();
    fs::create_dir_all(sbdir.join("migrations")).unwrap();
    fs::write(sbdir.join("migrations/001_init.sql"), "select 1;\n").unwrap();
    git(source.path(), &["add", "supabase"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add supabase",
        ],
    );
    let (_source, td) = init_managed_from(source);
    let bin = TempDir::new().unwrap();
    let log = td.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path())
        .env("PATH", &path)
        .env("MOCK_LOG", log)
        .args([
            "new",
            "x",
            "--install",
            "false",
            "--supabase",
            "isolated",
            "--db",
            "true",
        ]);
    cmd.assert().success();

    let wt_dir = worktree_path(&td, "x");
    let patched = fs::read_to_string(wt_dir.join("supabase").join("config.toml")).unwrap();

    // First allocation block is 1 => offset 100.
    assert!(patched.contains("project_id = \"myproj-x\""));
    assert!(patched.contains("port = 5532"));
    assert!(patched.contains("http://localhost:3100"));

    // Ensure skip-worktree is set.
    let v = git_out(&wt_dir, &["ls-files", "-v", "supabase/config.toml"]);
    assert!(v.starts_with('S'));

    let log = fs::read_to_string(td.path().join("supabase.log")).unwrap();
    assert!(
        log.lines()
            .any(|line| line.contains("supabase db reset") && line.contains("branch=x")),
        "{log}"
    );
}

#[test]
fn root_init_status_and_new_use_sibling_worktrees() {
    let source = init_repo();
    fs::write(
        source.path().join(".wrt.json"),
        r#"{
  "version": 1,
  "port_block_size": 100,
  "package_manager": { "name": "unknown", "install_command": ["npm","install"], "notes": null },
  "services": [{ "name": "web", "kind": "web", "dev_command": ["npm","run","dev"], "base_port": 3000, "port_env": "PORT", "url_env": "APP_URL", "notes": null }],
  "database": { "detected": false, "kind": null, "migrate_command": null, "seed_command": null, "reset_command": null, "notes": null },
  "supabase": { "detected": false, "config_path": null, "start_command": null, "base_ports": null, "notes": null },
  "notes": null
}
"#,
    )
    .unwrap();
    fs::write(source.path().join(".env"), "FOO=bar\n").unwrap();
    let managed = TempDir::new().unwrap();

    let mut cmd = wrt_cmd();
    cmd.current_dir(source.path()).args([
        "root",
        "init",
        source.path().to_str().unwrap(),
        "--root",
        managed.path().to_str().unwrap(),
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let main = managed.path().join("main");
    assert!(managed.path().join(".git").is_dir());
    assert!(managed.path().join(".wrt.json").exists());
    assert!(main.join("README.md").exists());
    assert!(main.join(".wrt.env").exists());
    assert_eq!(fs::read_to_string(main.join(".env")).unwrap(), "FOO=bar\n");
    assert_eq!(
        git_out(&main, &["config", "--get", "remote.origin.fetch"]).trim(),
        "+refs/heads/*:refs/remotes/origin/*"
    );

    wrt_cmd()
        .current_dir(managed.path())
        .args(["root", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("layout: managed-root"))
        .stdout(predicate::str::contains("tracked worktrees: 1"));

    wrt_cmd()
        .current_dir(&main)
        .args(["root", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("invocation worktree:"));

    let mut cmd = wrt_cmd();
    cmd.current_dir(managed.path()).args([
        "new",
        "feature/demo",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let feature = managed.path().join("feature-demo");
    assert!(feature.exists());
    assert_eq!(
        fs::read_to_string(feature.join(".env")).unwrap(),
        "FOO=bar\n"
    );
    assert!(feature.join(".wrt.json").exists());

    wrt_cmd()
        .current_dir(&feature)
        .args(["env"])
        .assert()
        .success()
        .stdout(predicate::str::contains("export WRT_NAME='feature-demo'"))
        .stdout(predicate::str::contains(format!(
            "export WRT_ROOT='{}'",
            managed.path().display()
        )))
        .stdout(predicate::str::contains(format!(
            "export WRT_MAIN_PATH='{}'",
            main.display()
        )))
        .stdout(predicate::str::contains("export PORT='3100'"))
        .stdout(predicate::str::contains(
            "export APP_URL='http://localhost:3100'",
        ));
}

#[test]
fn clone_derives_root_and_runs_managed_setup() {
    let source = init_repo();
    fs::write(source.path().join(".env"), "FOO=bar\n").unwrap();

    let parent = TempDir::new().unwrap();
    let root_name = source.path().file_name().unwrap().to_string_lossy();
    let managed = parent.path().join(root_name.as_ref());

    let mut cmd = wrt_cmd();
    cmd.current_dir(parent.path()).args([
        "clone",
        source.path().to_str().unwrap(),
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let main = managed.join("main");
    assert!(managed.join(".git").is_dir());
    assert!(main.join("README.md").exists());
    assert!(main.join(".wrt.env").exists());
    assert_eq!(fs::read_to_string(main.join(".env")).unwrap(), "FOO=bar\n");

    wrt_cmd()
        .current_dir(&managed)
        .args(["root", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("layout: managed-root"))
        .stdout(predicate::str::contains("tracked worktrees: 1"));
}

#[test]
fn clone_names_primary_worktree_after_default_branch() {
    let source = init_repo_on_branch("staging");

    let parent = TempDir::new().unwrap();
    let root_name = source.path().file_name().unwrap().to_string_lossy();
    let managed = parent.path().join(root_name.as_ref());

    let mut cmd = wrt_cmd();
    cmd.current_dir(parent.path()).args([
        "clone",
        source.path().to_str().unwrap(),
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let staging = managed.join("staging");
    assert!(managed.join(".git").is_dir());
    assert!(staging.join("README.md").exists());
    assert!(!managed.join("main").exists());
    assert_eq!(
        git_out(&staging, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "staging"
    );

    wrt_cmd()
        .current_dir(&managed)
        .args(["root", "status"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"main worktree: .*/staging\n").unwrap())
        .stdout(predicate::str::contains("tracked worktrees: 1"));

    wrt_cmd()
        .current_dir(&managed)
        .arg("ls")
        .assert()
        .success()
        .stdout(predicate::str::contains("staging"));
}

#[cfg(unix)]
#[test]
fn non_main_primary_owns_supabase_even_when_feature_is_named_main() {
    let source = init_repo_on_branch("staging");
    let supabase_dir = source.path().join("supabase");
    fs::create_dir_all(&supabase_dir).unwrap();
    fs::write(
        supabase_dir.join("config.toml"),
        "project_id = \"staging-project\"\n",
    )
    .unwrap();
    git(source.path(), &["add", "supabase/config.toml"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add supabase",
        ],
    );

    let managed = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    let log = managed.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    wrt_cmd()
        .current_dir(source.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "root",
            "init",
            source.path().to_str().unwrap(),
            "--root",
            managed.path().to_str().unwrap(),
            "--install",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "add",
            "main",
            "--install",
            "false",
            "--supabase",
            "none",
            "--db",
            "false",
        ])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(managed.path())
        .args(["root", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("supabase config: shared main"));

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "add",
            "feature/shared",
            "--install",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let state_path = managed.path().join(".git/.wrt/state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["allocations"]["staging"]["supabase"]["mode"], "owned");
    assert_eq!(state["allocations"]["main"]["supabase"]["mode"], "none");
    assert_eq!(
        state["allocations"]["feature-shared"]["supabase"]["owner"],
        "staging"
    );

    // Compatibility with state written before primary owners used their actual allocation key.
    state["allocations"]["feature-shared"]["supabase"]["owner"] =
        serde_json::Value::String("main".to_string());
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args(["env", "feature/shared"])
        .assert()
        .success()
        .stdout(predicate::str::contains("WRT_SUPABASE_OWNER='staging'"));

    wrt_cmd()
        .current_dir(managed.path())
        .args(["rm", "staging"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the main worktree cannot be removed",
        ));

    wrt_cmd()
        .current_dir(managed.path())
        .args(["rm", "main", "--force"])
        .assert()
        .success();
    assert!(!managed.path().join("main").exists());
}

#[cfg(unix)]
#[test]
fn new_runs_mocked_package_install_and_supabase_lifecycle() {
    let source = init_repo();
    fs::write(
        source.path().join("package.json"),
        r#"{"scripts":{"dev":"echo dev"}}"#,
    )
    .unwrap();
    fs::write(
        source.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    let sbdir = source.path().join("supabase");
    fs::create_dir_all(&sbdir).unwrap();
    fs::write(
        sbdir.join("config.toml"),
        "project_id = \"myproj\"\nport = 5432\n",
    )
    .unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add setup files",
        ],
    );
    let log = source.path().join("mock.log");
    let (_source, td) = init_managed_from(source);

    let bin = TempDir::new().unwrap();
    write_mock_bin(
        bin.path(),
        "pnpm",
        "#!/bin/sh\nprintf 'pnpm %s\\n' \"$*\" >> \"$MOCK_LOG\"\n",
    );
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    wrt_cmd()
        .current_dir(td.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .env("EXPECT_WRT_ENV", td.path().join("x/.wrt.env"))
        .args([
            "new",
            "x",
            "--install",
            "true",
            "--supabase",
            "true",
            "--db",
            "false",
        ])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(td.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args(["rm", "x", "--force"])
        .assert()
        .success();

    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("pnpm install"), "{log}");
    assert!(log.contains("supabase start"), "{log}");
    assert!(log.contains("supabase stop"), "{log}");
}

#[cfg(unix)]
#[test]
fn nested_supabase_config_supports_shared_and_isolated_features() {
    let source = init_repo();
    let project = source.path().join("apps").join("api");
    let sbdir = project.join("supabase");
    fs::create_dir_all(&sbdir).unwrap();
    fs::write(
        sbdir.join("config.toml"),
        "project_id = \"myproj\"\n[api]\nport = 54321\n[db]\nport = 54322\n",
    )
    .unwrap();
    fs::write(source.path().join(".env"), "TRACKED=value\n").unwrap();
    fs::create_dir_all(sbdir.join("migrations")).unwrap();
    fs::write(sbdir.join("migrations/001_init.sql"), "select 1;\n").unwrap();
    let alternate_config = "services/alt/supabase/config.toml";
    let alternate = source.path().join(alternate_config);
    fs::create_dir_all(alternate.parent().unwrap()).unwrap();
    fs::write(
        &alternate,
        "project_id = \"altproj\"\n[api]\nport = 55000\n",
    )
    .unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add nested supabase",
        ],
    );
    fs::write(project.join(".env"), "CUSTOM_NESTED=value\n").unwrap();

    let managed = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    let log = source.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    let config_path = "apps/api/supabase/config.toml";

    wrt_cmd()
        .current_dir(source.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "root",
            "init",
            source.path().to_str().unwrap(),
            "--root",
            managed.path().to_str().unwrap(),
            "--install",
            "false",
            "--supabase-config",
            config_path,
            "--db",
            "false",
        ])
        .assert()
        .success();

    let main = main_path(&managed);
    let main_config = fs::read_to_string(main.join(config_path)).unwrap();
    assert!(main_config.contains("project_id = \"myproj\""));
    assert!(!main_config.contains("myproj-main"));
    assert_eq!(
        fs::read_to_string(main.join(".env")).unwrap(),
        "TRACKED=value\n"
    );
    assert!(git_out(&main, &["diff", "--", ".env"]).is_empty());
    let main_env = fs::read_to_string(main.join(".env.local")).unwrap();
    assert!(main_env.contains("SUPABASE_URL='http://127.0.0.1:54321'"));
    let nested_env = fs::read_to_string(main.join("apps/api/.env")).unwrap();
    assert!(nested_env.contains("CUSTOM_NESTED=value"));
    assert!(nested_env.contains("SUPABASE_URL='http://127.0.0.1:54321'"));

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args(["add", "feature/shared", "--install", "false"])
        .assert()
        .success();

    let shared = worktree_path(&managed, "feature-shared");
    let shared_config = fs::read_to_string(shared.join(config_path)).unwrap();
    assert!(shared_config.contains("project_id = \"myproj\""));
    assert!(!shared_config.contains("myproj-feature-shared"));
    assert!(
        fs::read_to_string(shared.join(".wrt.env"))
            .unwrap()
            .contains("WRT_SUPABASE_OWNER='main'")
    );
    assert!(
        fs::read_to_string(shared.join("apps/api/.env"))
            .unwrap()
            .contains("CUSTOM_NESTED=value")
    );
    let state_path = managed.path().join(".git/.wrt/state.json");
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["root"]["supabaseConfigPath"], config_path);
    assert_eq!(state["allocations"]["main"]["supabase"]["mode"], "owned");
    assert_eq!(
        state["allocations"]["feature-shared"]["supabase"]["mode"],
        "shared"
    );
    assert!(
        !fs::read_to_string(&log)
            .unwrap()
            .contains("supabase db reset")
    );

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "add",
            "feature/shared-reset",
            "--install",
            "false",
            "--supabase",
            "shared",
            "--db",
            "true",
        ])
        .assert()
        .success();
    let reset_log = fs::read_to_string(&log).unwrap();
    assert!(reset_log.contains("supabase db reset"), "{reset_log}");
    assert!(
        reset_log
            .lines()
            .any(|line| line.contains("supabase db reset")
                && line.contains("/main/apps/api")
                && line.contains("branch=main")),
        "{reset_log}"
    );
    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args(["rm", "feature/shared-reset", "--force"])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args(["rm", "feature/shared", "--force"])
        .assert()
        .success();
    let shared_log = fs::read_to_string(&log).unwrap();
    assert_eq!(
        shared_log.matches("supabase start").count(),
        1,
        "{shared_log}"
    );
    assert_eq!(
        shared_log.matches("supabase stop").count(),
        0,
        "{shared_log}"
    );

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "add",
            "feature/conflict",
            "--install",
            "false",
            "--supabase",
            "shared",
            "--supabase-config",
            alternate_config,
            "--db",
            "false",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot override the main config in shared mode",
        ));
    assert!(!worktree_path(&managed, "feature-conflict").exists());

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args([
            "add",
            "feature/isolated",
            "--install",
            "false",
            "--supabase",
            "isolated",
            "--supabase-config",
            alternate_config,
            "--db",
            "false",
        ])
        .assert()
        .success();

    let isolated = worktree_path(&managed, "feature-isolated");
    let isolated_config = fs::read_to_string(isolated.join(alternate_config)).unwrap();
    assert!(isolated_config.contains("project_id = \"altproj-feature-isolated\""));
    assert!(isolated_config.contains("port = 55100"));
    let index = git_out(&isolated, &["ls-files", "-v", alternate_config]);
    assert!(index.starts_with('S'), "{index}");

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
        .args(["rm", "feature/isolated", "--force"])
        .assert()
        .success();
    let final_log = fs::read_to_string(log).unwrap();
    assert_eq!(
        final_log.matches("supabase start").count(),
        2,
        "{final_log}"
    );
    assert_eq!(final_log.matches("supabase stop").count(), 1, "{final_log}");
    assert!(
        final_log.contains("feature-isolated/services/alt"),
        "{final_log}"
    );
}

#[cfg(unix)]
#[test]
fn root_init_uses_supabase_path_from_local_wrt_config() {
    let source = init_repo();
    let config_path = "apps/api/supabase/config.toml";
    let config = source.path().join(config_path);
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "project_id = \"myproj\"\n").unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add nested supabase",
        ],
    );
    fs::write(
        source.path().join(".wrt.json"),
        format!(r#"{{"supabase":{{"config_path":"{config_path}"}}}}"#),
    )
    .unwrap();

    let managed = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    let log = source.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    wrt_cmd()
        .current_dir(source.path())
        .env("PATH", path)
        .env("MOCK_LOG", &log)
        .args([
            "root",
            "init",
            source.path().to_str().unwrap(),
            "--root",
            managed.path().to_str().unwrap(),
            "--install",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(managed.path().join(".git/.wrt/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["root"]["supabaseConfigPath"], config_path);
    assert!(fs::read_to_string(log).unwrap().contains("/main/apps/api"));
}

#[cfg(unix)]
#[test]
fn root_init_fails_when_detected_supabase_cannot_start() {
    let source = init_repo();
    let sbdir = source.path().join("supabase");
    fs::create_dir_all(&sbdir).unwrap();
    fs::write(sbdir.join("config.toml"), "project_id = \"myproj\"\n").unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add supabase",
        ],
    );
    let managed = TempDir::new().unwrap();

    let mut cmd = wrt_cmd();
    cmd.current_dir(source.path()).args([
        "root",
        "init",
        source.path().to_str().unwrap(),
        "--root",
        managed.path().to_str().unwrap(),
        "--install",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("supabase CLI not found"));

    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(managed.path().join(".git/.wrt/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["allocations"]["main"]["status"], "failed");
    assert_eq!(state["allocations"]["main"]["supabase"]["mode"], "owned");

    let bin = TempDir::new().unwrap();
    let log = managed.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", path)
        .env("MOCK_LOG", log)
        .args([
            "add",
            "recovery",
            "--install",
            "false",
            "--supabase",
            "shared",
            "--db",
            "false",
        ])
        .assert()
        .success();
    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(managed.path().join(".git/.wrt/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["allocations"]["main"]["status"], "failed");
    assert!(main_path(&managed).join(".env").exists());
}

#[test]
fn root_init_rejects_unsafe_supabase_config_path() {
    let source = init_repo();
    let managed = TempDir::new().unwrap();

    wrt_cmd()
        .current_dir(source.path())
        .args([
            "root",
            "init",
            source.path().to_str().unwrap(),
            "--root",
            managed.path().to_str().unwrap(),
            "--supabase-config",
            "../supabase/config.toml",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("must stay inside the worktree"));
    assert!(!managed.path().join(".git").exists());
}

#[test]
fn new_and_rm_roundtrip() {
    let (_source, td) = init_managed_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args([
            "new",
            "a/gpt/fix-login-timeout",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "a-gpt-fix-login-timeout");
    assert!(wt_dir.exists());
    assert!(wt_dir.join(".wrt.env").exists());

    wrt_cmd()
        .current_dir(td.path())
        .args(["rm", "a-gpt-fix-login-timeout", "--force"])
        .assert()
        .success();

    assert!(!wt_dir.exists());
}

#[cfg(unix)]
#[test]
fn new_concurrent_processes_reserve_disjoint_port_sets() {
    let source = init_repo();
    fs::write(
        source.path().join(".wrt.json"),
        complete_v2_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000},{"key":"postgres","base_port":5432}]}"#,
        ),
    )
    .unwrap();
    fs::create_dir_all(source.path().join("supabase")).unwrap();
    fs::write(
        source.path().join("supabase/config.toml"),
        "project_id = \"concurrent\"\n[api]\nport = 54321\n[db]\nport = 54322\n",
    )
    .unwrap();
    git(source.path(), &["add", ".wrt.json", "supabase/config.toml"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add concurrent port config",
        ],
    );
    let (_source, managed) = init_managed_from(source);
    let bin = TempDir::new().unwrap();
    let log = managed.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    let binary = assert_cmd::cargo::cargo_bin!("wrt");

    let spawn = |name: &str| {
        StdCommand::new(binary)
            .current_dir(managed.path())
            .env("PATH", &path)
            .env("MOCK_LOG", &log)
            .args([
                "new",
                name,
                "--install",
                "false",
                "--supabase",
                "isolated",
                "--db",
                "false",
            ])
            .spawn()
            .unwrap()
    };
    let first = spawn("concurrent-one");
    let second = spawn("concurrent-two");

    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(managed.path().join(".git/.wrt/state.json")).unwrap())
            .unwrap();
    let port_set = |name: &str| {
        state["allocations"][name]["ports"]
            .as_object()
            .unwrap()
            .values()
            .map(|value| value.as_u64().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let first = port_set("concurrent-one");
    let second = port_set("concurrent-two");
    assert_eq!(first.len(), 4);
    assert_eq!(second.len(), 4);
    assert!(first.is_disjoint(&second));
    for name in ["concurrent-one", "concurrent-two"] {
        let ports = state["allocations"][name]["ports"].as_object().unwrap();
        assert!(ports.contains_key("supabase.api.port"));
        assert!(ports.contains_key("supabase.db.port"));
    }
    assert_eq!(state["allocations"].as_object().unwrap().len(), 3);
}

#[test]
fn new_removes_checkout_after_config_or_reservation_failure() {
    let (_source, managed) = init_managed_repo();

    fs::write(managed.path().join(".wrt.json"), "not json").unwrap();
    wrt_cmd()
        .current_dir(managed.path())
        .args([
            "new",
            "invalid-config",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("project config failed"));
    assert!(!worktree_path(&managed, "invalid-config").exists());

    fs::write(
        managed.path().join(".wrt.json"),
        complete_v2_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":65500}]}"#,
        ),
    )
    .unwrap();
    wrt_cmd()
        .current_dir(managed.path())
        .args([
            "new",
            "invalid-reservation",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("reserve allocation failed"))
        .stderr(predicate::str::contains("out of range"));
    assert!(!worktree_path(&managed, "invalid-reservation").exists());

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(managed.path().join(".git/.wrt/state.json")).unwrap())
            .unwrap();
    assert!(state["allocations"]["invalid-config"].is_null());
    assert!(state["allocations"]["invalid-reservation"].is_null());
}

#[cfg(unix)]
#[test]
fn concurrent_shared_new_processes_converge_on_main_supabase_ownership() {
    let source = init_repo();
    fs::create_dir_all(source.path().join("supabase")).unwrap();
    fs::write(
        source.path().join("supabase/config.toml"),
        "project_id = \"main-project\"\n[api]\nport = 54321\n[db]\nport = 54322\n",
    )
    .unwrap();
    git(source.path(), &["add", "supabase/config.toml"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add supabase",
        ],
    );
    let (_source, managed) = init_managed_from(source);
    let bin = TempDir::new().unwrap();
    let log = managed.path().join("supabase.log");
    write_mock_supabase(bin.path());
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    let binary = assert_cmd::cargo::cargo_bin!("wrt");

    let spawn = |name: &str| {
        StdCommand::new(binary)
            .current_dir(managed.path())
            .env("PATH", &path)
            .env("MOCK_LOG", &log)
            .args([
                "new",
                name,
                "--install",
                "false",
                "--supabase",
                "shared",
                "--db",
                "false",
            ])
            .spawn()
            .unwrap()
    };
    let first = spawn("shared-one");
    let second = spawn("shared-two");

    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(managed.path().join(".git/.wrt/state.json")).unwrap())
            .unwrap();
    assert_eq!(state["allocations"]["main"]["supabase"]["mode"], "owned");
    assert_eq!(
        state["allocations"]["shared-one"]["supabase"]["owner"],
        "main"
    );
    assert_eq!(
        state["allocations"]["shared-two"]["supabase"]["owner"],
        "main"
    );
    assert_eq!(
        state["allocations"]["main"]["ports"]["supabase.api.port"],
        54321
    );
}

#[cfg(unix)]
#[test]
fn setup_project_command_retries_with_the_same_complete_environment() {
    let source = init_repo();
    fs::create_dir_all(source.path().join("scripts")).unwrap();
    fs::write(
        source.path().join(".wrt.json"),
        complete_v2_config(
            r#"{
  "version": 2,
  "port_stride": 100,
  "ports": [{
    "key": "core-api",
    "base_port": 3000,
    "outputs": [
      {"env": "CORE_API_PORT", "template": "{port}"},
      {"env": "DATABASE_URL", "template": "postgresql://localhost:{port}/core"}
    ]
  }],
  "commands": {
    "setup": {"argv": ["sh", "setup.sh"], "cwd": "scripts"},
    "db_reset": {"argv": ["sh", "setup.sh", "reset"], "cwd": "scripts"}
  },
  "supabase": {"config_path": "supabase/config.toml"}
}"#,
        ),
    )
    .unwrap();
    fs::write(
        source.path().join("scripts/setup.sh"),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = reset ]; then
  printf 'RESET\n' >> "$WRT_WORKTREE_PATH/setup.log"
  exit 0
fi
printf '%s\t%s\t%s\t%s\t%s\n' "$WRT_NAME" "$COMPOSE_PROJECT_NAME" "$CORE_API_PORT" "$WRT_SERVICE_CORE_API_PORT" "$DATABASE_URL" >> "$WRT_WORKTREE_PATH/setup.log"
test -f "$WRT_WORKTREE_PATH/.wrt.env"
grep -q '^COMPOSE_PROJECT_NAME=' "$WRT_WORKTREE_PATH/.wrt.env"
grep -q '^CORE_API_PORT=' "$WRT_WORKTREE_PATH/.wrt.env"
grep -q '^DATABASE_URL=' "$WRT_WORKTREE_PATH/.wrt.env"
if [ "$WRT_NAME" = retry-feature ] && [ ! -f "$WRT_WORKTREE_PATH/.setup-attempted" ]; then
  touch "$WRT_WORKTREE_PATH/.setup-attempted"
  exit 23
fi
"#,
    )
    .unwrap();
    fs::write(
        source.path().join("package.json"),
        r#"{"scripts":{"setup":"false"}}"#,
    )
    .unwrap();
    fs::write(
        source.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    fs::create_dir_all(source.path().join("supabase")).unwrap();
    fs::write(
        source.path().join("supabase/config.toml"),
        "project_id = \"project\"\n[api]\nport = 54321\n",
    )
    .unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add project setup",
        ],
    );
    let (_source, managed) = init_managed_from(source);

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", "/usr/bin:/bin")
        .args([
            "new",
            "retry-feature",
            "--install",
            "true",
            "--supabase",
            "isolated",
            "--db",
            "true",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "project setup command exited with status 23",
        ));

    let worktree = worktree_path(&managed, "retry-feature");
    let state_path = managed.path().join(".git/.wrt/state.json");
    let failed_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let failed_ports = failed_state["allocations"]["retry-feature"]["ports"].clone();
    let failed_generation = failed_state["allocations"]["retry-feature"]["generationId"].clone();
    assert_eq!(
        failed_state["allocations"]["retry-feature"]["status"],
        "failed"
    );

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", "/usr/bin:/bin")
        .args(["setup", "retry/feature"])
        .assert()
        .success();

    let active_state: serde_json::Value =
        serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
    assert_eq!(
        active_state["allocations"]["retry-feature"]["ports"],
        failed_ports
    );
    assert_eq!(
        active_state["allocations"]["retry-feature"]["generationId"],
        failed_generation
    );
    assert_eq!(
        active_state["allocations"]["retry-feature"]["status"],
        "active"
    );

    let log = fs::read_to_string(worktree.join("setup.log")).unwrap();
    let attempts = log.lines().collect::<Vec<_>>();
    assert_eq!(attempts.len(), 2, "{log}");
    assert_eq!(attempts[0], attempts[1], "{log}");
    assert!(attempts[0].starts_with("retry-feature\twrt-"), "{log}");
    assert!(attempts[0].contains("\t3100\t3100\t"), "{log}");
    assert!(
        attempts[0].ends_with("postgresql://localhost:3100/core"),
        "{log}"
    );
    assert!(!log.contains("RESET"), "{log}");
    let supabase_config = fs::read_to_string(worktree.join("supabase/config.toml")).unwrap();
    assert!(supabase_config.contains("project_id = \"project\""));
    assert!(supabase_config.contains("port = 54321"));
}

#[cfg(unix)]
#[test]
fn setup_legacy_flow_writes_environment_before_install() {
    let source = init_repo();
    fs::write(
        source.path().join("package.json"),
        r#"{"scripts":{"dev":"echo dev"}}"#,
    )
    .unwrap();
    fs::write(
        source.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "add package",
        ],
    );
    let (_source, managed) = init_managed_from(source);
    let bin = TempDir::new().unwrap();
    write_mock_bin(
        bin.path(),
        "pnpm",
        "#!/bin/sh\ntest -f .wrt.env\ngrep -q '^COMPOSE_PROJECT_NAME=' .wrt.env\nprintf 'installed\\n' >> install.log\n",
    );
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .args([
            "new",
            "legacy-setup",
            "--install",
            "true",
            "--supabase",
            "none",
            "--db",
            "false",
        ])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(managed.path())
        .env("PATH", &path)
        .args(["setup", "legacy-setup"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(worktree_path(&managed, "legacy-setup").join("install.log")).unwrap(),
        "installed\ninstalled\n"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(managed.path().join(".git/.wrt/state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["allocations"]["legacy-setup"]["setup"]["install"],
        "true"
    );
    assert_eq!(state["allocations"]["legacy-setup"]["setup"]["db"], "false");
}

#[test]
fn add_and_remove_aliases_roundtrip() {
    let (_source, td) = init_managed_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args([
            "add",
            "x",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "x");
    assert!(wt_dir.exists());

    wrt_cmd()
        .current_dir(td.path())
        .args(["remove", "x", "--force"])
        .assert()
        .success();

    assert!(!wt_dir.exists());
}

#[test]
fn new_works_from_bare_managed_root_even_when_main_checkout_is_missing() {
    let (_source, td) = init_bare_managed_without_main();

    wrt_cmd()
        .current_dir(td.path())
        .args([
            "new",
            "staging",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "staging");
    assert!(wt_dir.exists());
    assert!(wt_dir.join(".wrt.env").exists());
}

#[cfg(unix)]
#[test]
fn init_from_bare_managed_root_uses_existing_checkout_when_main_is_missing() {
    let (_source, td) = init_bare_managed_with_staging_checkout_without_main();

    let bin = TempDir::new().unwrap();
    let pwd_log = td.path().join("codex-pwd.log");
    write_mock_bin(
        bin.path(),
        "codex",
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift || break
done
printf '%s\n' "$PWD" > "$CODEX_PWD_LOG"
cat > "$out" <<'JSON'
{
  "version": 1,
  "port_block_size": 100,
  "package_manager": { "name": "unknown", "install_command": [], "notes": null },
  "services": [],
  "database": { "detected": false, "kind": null, "migrate_command": null, "seed_command": null, "reset_command": null, "notes": null },
  "supabase": { "detected": false, "config_path": null, "start_command": null, "base_ports": null, "notes": null },
  "notes": null
}
JSON
"#,
    );

    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    wrt_cmd()
        .current_dir(td.path())
        .env("PATH", path)
        .env("CODEX_PWD_LOG", &pwd_log)
        .args(["init"])
        .assert()
        .success();

    let staging = worktree_path(&td, "staging");
    let actual_pwd = PathBuf::from(fs::read_to_string(pwd_log).unwrap().trim()).canonicalize();
    assert_eq!(actual_pwd.unwrap(), staging.canonicalize().unwrap());
    assert!(td.path().join(".wrt.json").exists());
    assert!(!staging.join(".wrt.json").exists());
    assert!(!main_path(&td).join(".wrt.json").exists());
}

#[test]
fn new_uses_upstream_branch_when_present() {
    let source = init_repo();
    let origin = TempDir::new().unwrap();

    git(origin.path(), &["init", "--bare"]);
    git(
        source.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );

    let main = git_out(source.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();
    git(source.path(), &["push", "-u", "origin", main]);

    git(source.path(), &["checkout", "-b", "feature/upstream"]);
    fs::write(source.path().join("FEATURE.txt"), "hello\n").unwrap();
    git(source.path(), &["add", "FEATURE.txt"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "feature",
        ],
    );
    git(source.path(), &["push", "-u", "origin", "feature/upstream"]);

    git(source.path(), &["checkout", main]);
    git(source.path(), &["branch", "-D", "feature/upstream"]);
    let (_source, td) = init_managed_from(source);
    let main_path = main_path(&td);
    set_origin(&main_path, origin.path());

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args([
        "new",
        "feature/upstream",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let wt_dir = worktree_path(&td, "feature-upstream");
    assert!(wt_dir.join("FEATURE.txt").exists());

    let upstream = git_out(
        &main_path,
        &["rev-parse", "--abbrev-ref", "feature/upstream@{upstream}"],
    );
    assert_eq!(upstream.trim(), "origin/feature/upstream");
}

#[test]
fn new_replaces_untracked_local_branch_with_remote_branch() {
    let source = init_repo();
    let origin = TempDir::new().unwrap();

    git(origin.path(), &["init", "--bare"]);
    git(
        source.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );

    let main = git_out(source.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();
    git(source.path(), &["push", "-u", "origin", main]);

    git(source.path(), &["checkout", "-b", "feature/existing"]);
    fs::write(source.path().join("FEATURE.txt"), "hello\n").unwrap();
    git(source.path(), &["add", "FEATURE.txt"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "feature",
        ],
    );
    git(source.path(), &["push", "-u", "origin", "feature/existing"]);

    git(source.path(), &["checkout", main]);
    git(source.path(), &["branch", "-D", "feature/existing"]);
    git(source.path(), &["checkout", "-b", "feature/existing"]);
    fs::write(source.path().join("STALE.txt"), "stale\n").unwrap();
    git(source.path(), &["add", "STALE.txt"]);
    git(
        source.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "stale",
        ],
    );
    git(source.path(), &["checkout", main]);
    let (_source, td) = init_managed_from(source);
    let main_path = main_path(&td);
    set_origin_with_remote_tracking(&main_path, origin.path());
    git(&main_path, &["fetch", "origin"]);

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args([
        "add",
        "feature/existing",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let upstream = git_out(
        &main_path,
        &["rev-parse", "--abbrev-ref", "feature/existing@{upstream}"],
    );
    assert_eq!(upstream.trim(), "origin/feature/existing");
    assert!(
        worktree_path(&td, "feature-existing")
            .join("FEATURE.txt")
            .exists()
    );
    assert!(
        !worktree_path(&td, "feature-existing")
            .join("STALE.txt")
            .exists()
    );
    assert_eq!(
        git_out(&main_path, &["rev-parse", "feature/existing"]),
        git_out(&main_path, &["rev-parse", "origin/feature/existing"])
    );
}

#[test]
fn new_copies_repo_env_when_present() {
    let source = init_repo();

    fs::write(source.path().join(".env"), "FOO=bar\n").unwrap();
    let (_source, td) = init_managed_from(source);

    wrt_cmd()
        .current_dir(td.path())
        .args([
            "new",
            "x",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let wt_env = worktree_path(&td, "x").join(".env");
    assert!(wt_env.exists());
    assert_eq!(fs::read_to_string(wt_env).unwrap(), "FOO=bar\n");
}

#[test]
fn new_cd_prints_shell_cd_snippet() {
    let (_source, td) = init_managed_repo();

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args([
        "new",
        "x",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "false",
        "--cd",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("cd '").and(predicate::str::contains("/x'")));

    let wt_dir = worktree_path(&td, "x");
    assert!(wt_dir.exists());
}

#[test]
fn new_db_auto_skips_non_interactive_and_true_runs() {
    let source = init_repo();

    // Repo-local config with a db reset command.
    fs::write(
        source.path().join(".wrt.json"),
        r#"{
  "version": 1,
  "port_block_size": 100,
  "package_manager": { "name": "unknown", "install_command": ["npm","install"], "notes": null },
  "services": [],
	  "database": {
	    "detected": true,
	    "kind": "unknown",
	    "migrate_command": null,
	    "seed_command": null,
	    "reset_command": ["sh","-c","echo ran > .db_ran"],
	    "notes": null
	  },
  "supabase": { "detected": false, "config_path": null, "start_command": null, "base_ports": null, "notes": null },
  "notes": null
}
"#,
    )
    .unwrap();
    let (_source, td) = init_managed_from(source);

    // auto: stdin isn't a tty in tests, so it must not run the command.
    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args([
        "new",
        "x",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "auto",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("skipping in non-interactive mode"));

    let wt_dir = worktree_path(&td, "x");
    assert!(!wt_dir.join(".db_ran").exists());

    // true: should run without prompting (still non-interactive).
    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args([
        "new",
        "y",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "true",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let wt_dir = worktree_path(&td, "y");
    assert!(wt_dir.join(".db_ran").exists());
}

#[test]
fn new_db_true_does_not_fallback_to_seed_or_migrate() {
    let source = init_repo();

    // Only seed is present; `wrt new --db true` should not run it.
    fs::write(
        source.path().join(".wrt.json"),
        r#"{
  "version": 1,
  "port_block_size": 100,
  "package_manager": { "name": "unknown", "install_command": ["npm","install"], "notes": null },
  "services": [],
	  "database": {
	    "detected": true,
	    "kind": "unknown",
	    "migrate_command": null,
	    "seed_command": ["sh","-c","echo ran > .db_seed_ran"],
	    "reset_command": null,
	    "notes": null
	  },
  "supabase": { "detected": false, "config_path": null, "start_command": null, "base_ports": null, "notes": null },
  "notes": null
}
"#,
    )
    .unwrap();
    let (_source, td) = init_managed_from(source);

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args([
        "new",
        "x",
        "--install",
        "false",
        "--supabase",
        "false",
        "--db",
        "true",
    ]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let wt_dir = worktree_path(&td, "x");
    assert!(!wt_dir.join(".db_seed_ran").exists());
}

#[test]
fn db_reset_requires_yes_non_interactive_and_runs_with_yes() {
    let source = init_repo();

    fs::write(
        source.path().join(".wrt.json"),
        r#"{
  "version": 1,
  "port_block_size": 100,
  "package_manager": { "name": "unknown", "install_command": ["npm","install"], "notes": null },
  "services": [],
	  "database": {
	    "detected": true,
	    "kind": "unknown",
	    "migrate_command": null,
	    "seed_command": null,
	    "reset_command": ["sh","-c","echo ran > .db_ran"],
	    "notes": null
	  },
  "supabase": { "detected": false, "config_path": null, "start_command": null, "base_ports": null, "notes": null },
  "notes": null
}
"#,
    )
    .unwrap();
    let (_source, td) = init_managed_from(source);

    wrt_cmd()
        .current_dir(td.path())
        .args([
            "new",
            "x",
            "--install",
            "false",
            "--supabase",
            "false",
            "--db",
            "false",
        ])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "x");

    // Non-interactive test: must refuse without --yes.
    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).args(["db", "x", "reset"]);
    set_minimal_path(&mut cmd);
    cmd.assert().code(2).stderr(predicate::str::contains(
        "refusing to run reset non-interactively",
    ));
    assert!(!wt_dir.join(".db_ran").exists());

    // With --yes, should run.
    let mut cmd = wrt_cmd();
    // Run from inside the worktree without passing <name>; should infer it.
    cmd.current_dir(&wt_dir).args(["db", "reset", "--yes"]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();
    assert!(wt_dir.join(".db_ran").exists());
}

#[test]
fn rm_delete_branch_removes_branch_ref() {
    let (_source, td) = init_managed_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    wrt_cmd()
        .current_dir(td.path())
        .args(["rm", "x", "--force", "--delete-branch"])
        .assert()
        .success();

    let status = StdCommand::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/x"])
        .current_dir(main_path(&td))
        .status()
        .unwrap();
    assert!(!status.success());
}

#[test]
fn rm_delete_branch_removes_configured_remote_branch() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    let origin = TempDir::new().unwrap();
    git(origin.path(), &["init", "--bare"]);
    set_origin_with_remote_tracking(&main, origin.path());
    git(&main, &["push", "-u", "origin", "main"]);

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "x");
    git(&wt_dir, &["push", "-u", "origin", "x"]);

    wrt_cmd()
        .current_dir(td.path())
        .args(["rm", "x", "--force", "--delete-branch"])
        .assert()
        .success()
        .stderr(predicate::str::contains("deleting remote branch: origin/x"));

    let local_status = StdCommand::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/x"])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(!local_status.success());

    let remote_status = StdCommand::new("git")
        .args([
            "ls-remote",
            "--exit-code",
            "--heads",
            "origin",
            "refs/heads/x",
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(!remote_status.success());
}

#[test]
fn rm_non_interactive_keeps_local_and_remote_branches_by_default() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    let origin = TempDir::new().unwrap();
    git(origin.path(), &["init", "--bare"]);
    set_origin_with_remote_tracking(&main, origin.path());
    git(&main, &["push", "-u", "origin", "main"]);

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "x");
    git(&wt_dir, &["push", "-u", "origin", "x"]);

    wrt_cmd()
        .current_dir(td.path())
        .args(["rm", "x", "--force"])
        .assert()
        .success();

    let local_status = StdCommand::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/x"])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(local_status.success());

    let remote_status = StdCommand::new("git")
        .args([
            "ls-remote",
            "--exit-code",
            "--heads",
            "origin",
            "refs/heads/x",
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(remote_status.success());
}

#[test]
fn env_infers_from_cwd() {
    let (_source, td) = init_managed_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "x");

    wrt_cmd()
        .current_dir(&wt_dir)
        .args(["env"])
        .assert()
        .success()
        .stdout(predicate::str::contains("export WRT_NAME='x'"));
}

#[test]
fn prune_removes_missing_worktrees_from_state() {
    let (_source, td) = init_managed_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    let wt_dir = worktree_path(&td, "x");
    fs::remove_dir_all(&wt_dir).unwrap();
    assert!(!wt_dir.exists());

    wrt_cmd()
        .current_dir(td.path())
        .args(["prune"])
        .assert()
        .success();

    let st_path = td.path().join(".git").join(".wrt").join("state.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(st_path).unwrap()).unwrap();
    let allocs = v.get("allocations").unwrap().as_object().unwrap();
    assert!(!allocs.contains_key("x"));
}

#[test]
fn housekeeping_dry_run_lists_merged_unattached_branches() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    git(&main, &["branch", "-M", "main"]);

    git(&main, &["checkout", "-b", "feature/local"]);
    fs::write(main.join("LOCAL.txt"), "local\n").unwrap();
    git(&main, &["add", "LOCAL.txt"]);
    git(
        &main,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "local",
        ],
    );
    git(&main, &["checkout", "main"]);
    git(
        &main,
        &["merge", "--no-ff", "feature/local", "-m", "merge local"],
    );

    git(&main, &["checkout", "-b", "feature/attached"]);
    fs::write(main.join("ATTACHED.txt"), "attached\n").unwrap();
    git(&main, &["add", "ATTACHED.txt"]);
    git(
        &main,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "attached",
        ],
    );
    git(&main, &["checkout", "main"]);
    git(
        &main,
        &[
            "merge",
            "--no-ff",
            "feature/attached",
            "-m",
            "merge attached",
        ],
    );
    let attached = worktree_path(&td, "attached");
    git(
        &main,
        &[
            "worktree",
            "add",
            attached.to_str().unwrap(),
            "feature/attached",
        ],
    );

    wrt_cmd()
        .current_dir(td.path())
        .arg("housekeeping")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "local feature/local: merged into main, not attached to a worktree",
        ))
        .stdout(predicate::str::contains("feature/attached").not());
}

#[test]
fn housekeeping_apply_deletes_local_and_remote_candidates() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    let origin = TempDir::new().unwrap();
    git(&main, &["branch", "-M", "main"]);
    git(origin.path(), &["init", "--bare"]);
    set_origin_with_remote_tracking(&main, origin.path());
    git(&main, &["push", "-u", "origin", "main"]);

    git(&main, &["checkout", "-b", "feature/local"]);
    fs::write(main.join("LOCAL.txt"), "local\n").unwrap();
    git(&main, &["add", "LOCAL.txt"]);
    git(
        &main,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "local",
        ],
    );
    git(&main, &["checkout", "main"]);
    git(
        &main,
        &["merge", "--no-ff", "feature/local", "-m", "merge local"],
    );

    git(&main, &["checkout", "-b", "feature/remote"]);
    fs::write(main.join("REMOTE.txt"), "remote\n").unwrap();
    git(&main, &["add", "REMOTE.txt"]);
    git(
        &main,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "remote",
        ],
    );
    git(&main, &["push", "-u", "origin", "feature/remote"]);
    git(&main, &["checkout", "main"]);
    git(
        &main,
        &["merge", "--no-ff", "feature/remote", "-m", "merge remote"],
    );
    git(&main, &["push", "origin", "main"]);
    git(&main, &["branch", "-D", "feature/remote"]);
    git(&main, &["fetch", "origin"]);

    wrt_cmd()
        .current_dir(td.path())
        .args(["housekeeping", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "local feature/local: merged into main, not attached to a worktree",
        ))
        .stdout(predicate::str::contains(
            "remote origin/feature/remote: merged into main, not attached to a worktree",
        ));

    let local = StdCommand::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/feature/local",
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(!local.success());

    let remote = StdCommand::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/remotes/origin/feature/remote",
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(!remote.success());
}

#[test]
fn housekeeping_apply_warns_and_continues_when_remote_ref_is_stale() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    let origin = TempDir::new().unwrap();
    git(&main, &["branch", "-M", "main"]);
    git(origin.path(), &["init", "--bare"]);
    set_origin_with_remote_tracking(&main, origin.path());
    git(&main, &["push", "-u", "origin", "main"]);

    git(&main, &["checkout", "-b", "feature/stale"]);
    fs::write(main.join("STALE.txt"), "stale\n").unwrap();
    git(&main, &["add", "STALE.txt"]);
    git(
        &main,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "stale",
        ],
    );
    git(&main, &["push", "-u", "origin", "feature/stale"]);
    git(&main, &["checkout", "main"]);
    git(
        &main,
        &["merge", "--no-ff", "feature/stale", "-m", "merge stale"],
    );
    git(&main, &["push", "origin", "main"]);
    git(&main, &["branch", "-D", "feature/stale"]);
    git(&main, &["fetch", "origin"]);
    git(
        origin.path(),
        &["update-ref", "-d", "refs/heads/feature/stale"],
    );

    wrt_cmd()
        .current_dir(td.path())
        .args(["housekeeping", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "remote origin/feature/stale: merged into main, not attached to a worktree",
        ))
        .stderr(predicate::str::contains(
            "warning: skipped remote origin/feature/stale",
        ));
}

#[test]
fn housekeeping_prints_nothing_to_clean() {
    let (_source, td) = init_managed_repo();
    let main = main_path(&td);
    git(&main, &["branch", "-M", "main"]);

    wrt_cmd()
        .current_dir(td.path())
        .arg("housekeeping")
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to clean"));
}

#[test]
fn run_propagates_exit_code_and_requires_separator() {
    let (_source, td) = init_managed_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    // Missing `--` should return code 2.
    wrt_cmd()
        .current_dir(td.path())
        .args(["run", "x", "sh", "-c", "exit 7"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("usage: wrt run"));

    // With `--`, should run and propagate the exit code.
    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path())
        .args(["run", "x", "--", "sh", "-c", "exit 42"]);
    set_minimal_path(&mut cmd);
    cmd.assert().code(42);
}
