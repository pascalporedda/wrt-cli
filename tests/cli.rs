use predicates::prelude::*;
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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn init_repo() -> TempDir {
    let td = TempDir::new().unwrap();
    git(td.path(), &["init"]);
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

fn wrt_cmd() -> assert_cmd::Command {
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("wrt"))
}

fn set_minimal_path(cmd: &mut assert_cmd::Command) {
    // Ensure git/sh are present but supabase is very unlikely.
    cmd.env("PATH", "/usr/bin:/bin");
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
fn ls_empty() {
    let td = init_repo();

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path()).arg("ls");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("(no worktrees tracked by wrt)"));

    let exclude = td.path().join(".git").join("info").join("exclude");
    let ex = fs::read_to_string(exclude).unwrap();
    assert!(ex.lines().any(|l| l.trim() == ".worktrees/"));
    assert!(ex.lines().any(|l| l.trim() == ".wrt.env"));
    assert!(ex.lines().any(|l| l.trim() == ".wrt.json"));
}

#[test]
fn init_print_uses_mock_output() {
    let td = init_repo();

    let mock = td.path().join("mock.json");
    fs::write(
        &mock,
        r#"{"version":1,"port_block_size":100,"package_manager":{"name":"unknown","install_command":["npm","install"]},"services":[],"supabase":{"detected":false}}"#,
    )
    .unwrap();

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path())
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init", "--print"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("\"version\": 1"));

    assert!(!td.path().join(".wrt.json").exists());
}

#[test]
fn init_writes_config_and_respects_force() {
    let td = init_repo();

    let mock = td.path().join("mock.json");
    fs::write(
        &mock,
        r#"{"version":1,"port_block_size":100,"package_manager":{"name":"unknown","install_command":["npm","install"]},"services":[],"supabase":{"detected":false}}"#,
    )
    .unwrap();

    wrt_cmd()
        .current_dir(td.path())
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init"])
        .assert()
        .success();

    let out_path = td.path().join(".wrt.json");
    assert!(out_path.exists());
    let s = fs::read_to_string(&out_path).unwrap();
    assert!(s.contains("\"version\": 1"));
    assert!(s.ends_with('\n'));

    // Without --force, should refuse overwrite.
    wrt_cmd()
        .current_dir(td.path())
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists"));

    // With --force, should overwrite.
    wrt_cmd()
        .current_dir(td.path())
        .env("WRT_CODEX_MOCK_OUTPUT", &mock)
        .args(["init", "--force"])
        .assert()
        .success();
}

#[test]
fn new_patches_supabase_and_sets_skip_worktree_when_auto() {
    let td = init_repo();

    let sbdir = td.path().join("supabase");
    fs::create_dir_all(&sbdir).unwrap();
    fs::write(
        sbdir.join("config.toml"),
        "project_id = \"myproj\"\nport = 5432\nauth_site_url = \"http://localhost:3000\"\n",
    )
    .unwrap();
    git(td.path(), &["add", "supabase/config.toml"]);
    git(
        td.path(),
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

    let mut cmd = wrt_cmd();
    cmd.current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "auto"]);
    set_minimal_path(&mut cmd);
    cmd.assert().success();

    let wt_dir = td.path().join(".worktrees").join("x");
    let patched = fs::read_to_string(wt_dir.join("supabase").join("config.toml")).unwrap();

    // First allocation block is 1 => offset 100.
    assert!(patched.contains("project_id = \"myproj-x\""));
    assert!(patched.contains("port = 5532"));
    assert!(patched.contains("http://localhost:3100"));

    // Ensure skip-worktree is set.
    let v = git_out(&wt_dir, &["ls-files", "-v", "supabase/config.toml"]);
    assert!(v.starts_with('S'));
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
    assert!(main.join("README.md").exists());
    assert!(main.join(".wrt.env").exists());
    assert_eq!(fs::read_to_string(main.join(".env")).unwrap(), "FOO=bar\n");

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
    assert!(!main.join(".worktrees").join("feature-demo").exists());
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

#[cfg(unix)]
#[test]
fn new_runs_mocked_package_install_and_supabase_lifecycle() {
    let td = init_repo();
    fs::write(
        td.path().join("package.json"),
        r#"{"scripts":{"dev":"echo dev"}}"#,
    )
    .unwrap();
    fs::write(td.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    let sbdir = td.path().join("supabase");
    fs::create_dir_all(&sbdir).unwrap();
    fs::write(
        sbdir.join("config.toml"),
        "project_id = \"myproj\"\nport = 5432\n",
    )
    .unwrap();
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
            "add setup files",
        ],
    );

    let bin = TempDir::new().unwrap();
    let log = td.path().join("mock.log");
    write_mock_bin(
        bin.path(),
        "pnpm",
        "#!/bin/sh\nprintf 'pnpm %s\\n' \"$*\" >> \"$MOCK_LOG\"\n",
    );
    write_mock_bin(
        bin.path(),
        "supabase",
        "#!/bin/sh\nprintf 'supabase %s\\n' \"$*\" >> \"$MOCK_LOG\"\n",
    );
    let path = format!("{}:/usr/bin:/bin", bin.path().display());

    wrt_cmd()
        .current_dir(td.path())
        .env("PATH", &path)
        .env("MOCK_LOG", &log)
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

#[test]
fn new_and_rm_roundtrip() {
    let td = init_repo();

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

    let wt_dir = td.path().join(".worktrees").join("a-gpt-fix-login-timeout");
    assert!(wt_dir.exists());
    assert!(wt_dir.join(".wrt.env").exists());

    wrt_cmd()
        .current_dir(td.path())
        .args(["rm", "a-gpt-fix-login-timeout", "--force"])
        .assert()
        .success();

    assert!(!wt_dir.exists());
}

#[test]
fn new_uses_upstream_branch_when_present() {
    let td = init_repo();
    let origin = TempDir::new().unwrap();

    git(origin.path(), &["init", "--bare"]);
    git(
        td.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );

    let main = git_out(td.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();
    git(td.path(), &["push", "-u", "origin", main]);

    git(td.path(), &["checkout", "-b", "feature/upstream"]);
    fs::write(td.path().join("FEATURE.txt"), "hello\n").unwrap();
    git(td.path(), &["add", "FEATURE.txt"]);
    git(
        td.path(),
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
    git(td.path(), &["push", "-u", "origin", "feature/upstream"]);

    git(td.path(), &["checkout", main]);
    git(td.path(), &["branch", "-D", "feature/upstream"]);

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

    let wt_dir = td.path().join(".worktrees").join("feature-upstream");
    assert!(wt_dir.join("FEATURE.txt").exists());

    let upstream = git_out(
        td.path(),
        &["rev-parse", "--abbrev-ref", "feature/upstream@{upstream}"],
    );
    assert_eq!(upstream.trim(), "origin/feature/upstream");
}

#[test]
fn new_copies_repo_env_when_present() {
    let td = init_repo();

    fs::write(td.path().join(".env"), "FOO=bar\n").unwrap();

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

    let wt_env = td.path().join(".worktrees").join("x").join(".env");
    assert!(wt_env.exists());
    assert_eq!(fs::read_to_string(wt_env).unwrap(), "FOO=bar\n");
}

#[test]
fn new_cd_prints_shell_cd_snippet() {
    let td = init_repo();

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
        .stdout(predicate::str::contains("cd '").and(predicate::str::contains("/.worktrees/x'")));

    let wt_dir = td.path().join(".worktrees").join("x");
    assert!(wt_dir.exists());
}

#[test]
fn new_db_auto_skips_non_interactive_and_true_runs() {
    let td = init_repo();

    // Repo-local config with a db reset command.
    fs::write(
        td.path().join(".wrt.json"),
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

    let wt_dir = td.path().join(".worktrees").join("x");
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

    let wt_dir = td.path().join(".worktrees").join("y");
    assert!(wt_dir.join(".db_ran").exists());
}

#[test]
fn new_db_true_does_not_fallback_to_seed_or_migrate() {
    let td = init_repo();

    // Only seed is present; `wrt new --db true` should not run it.
    fs::write(
        td.path().join(".wrt.json"),
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

    let wt_dir = td.path().join(".worktrees").join("x");
    assert!(!wt_dir.join(".db_seed_ran").exists());
}

#[test]
fn db_reset_requires_yes_non_interactive_and_runs_with_yes() {
    let td = init_repo();

    fs::write(
        td.path().join(".wrt.json"),
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

    let wt_dir = td.path().join(".worktrees").join("x");

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
    let td = init_repo();

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
        .current_dir(td.path())
        .status()
        .unwrap();
    assert!(!status.success());
}

#[test]
fn env_infers_from_cwd() {
    let td = init_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    let wt_dir = td.path().join(".worktrees").join("x");

    wrt_cmd()
        .current_dir(&wt_dir)
        .args(["env"])
        .assert()
        .success()
        .stdout(predicate::str::contains("export WRT_NAME='x'"));
}

#[test]
fn prune_removes_missing_worktrees_from_state() {
    let td = init_repo();

    wrt_cmd()
        .current_dir(td.path())
        .args(["new", "x", "--install", "false", "--supabase", "false"])
        .assert()
        .success();

    let wt_dir = td.path().join(".worktrees").join("x");
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
    let td = init_repo();
    git(td.path(), &["branch", "-M", "main"]);

    git(td.path(), &["checkout", "-b", "feature/local"]);
    fs::write(td.path().join("LOCAL.txt"), "local\n").unwrap();
    git(td.path(), &["add", "LOCAL.txt"]);
    git(
        td.path(),
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
    git(td.path(), &["checkout", "main"]);
    git(
        td.path(),
        &["merge", "--no-ff", "feature/local", "-m", "merge local"],
    );

    git(td.path(), &["checkout", "-b", "feature/attached"]);
    fs::write(td.path().join("ATTACHED.txt"), "attached\n").unwrap();
    git(td.path(), &["add", "ATTACHED.txt"]);
    git(
        td.path(),
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
    git(td.path(), &["checkout", "main"]);
    git(
        td.path(),
        &[
            "merge",
            "--no-ff",
            "feature/attached",
            "-m",
            "merge attached",
        ],
    );
    git(
        td.path(),
        &["worktree", "add", ".worktrees/attached", "feature/attached"],
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
    let td = init_repo();
    let origin = TempDir::new().unwrap();
    git(td.path(), &["branch", "-M", "main"]);
    git(origin.path(), &["init", "--bare"]);
    git(
        td.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    git(td.path(), &["push", "-u", "origin", "main"]);

    git(td.path(), &["checkout", "-b", "feature/local"]);
    fs::write(td.path().join("LOCAL.txt"), "local\n").unwrap();
    git(td.path(), &["add", "LOCAL.txt"]);
    git(
        td.path(),
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
    git(td.path(), &["checkout", "main"]);
    git(
        td.path(),
        &["merge", "--no-ff", "feature/local", "-m", "merge local"],
    );

    git(td.path(), &["checkout", "-b", "feature/remote"]);
    fs::write(td.path().join("REMOTE.txt"), "remote\n").unwrap();
    git(td.path(), &["add", "REMOTE.txt"]);
    git(
        td.path(),
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
    git(td.path(), &["push", "-u", "origin", "feature/remote"]);
    git(td.path(), &["checkout", "main"]);
    git(
        td.path(),
        &["merge", "--no-ff", "feature/remote", "-m", "merge remote"],
    );
    git(td.path(), &["push", "origin", "main"]);
    git(td.path(), &["branch", "-D", "feature/remote"]);

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
        .current_dir(td.path())
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
        .current_dir(td.path())
        .status()
        .unwrap();
    assert!(!remote.success());
}

#[test]
fn housekeeping_apply_warns_and_continues_when_remote_ref_is_stale() {
    let td = init_repo();
    let origin = TempDir::new().unwrap();
    git(td.path(), &["branch", "-M", "main"]);
    git(origin.path(), &["init", "--bare"]);
    git(
        td.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    git(td.path(), &["push", "-u", "origin", "main"]);

    git(td.path(), &["checkout", "-b", "feature/stale"]);
    fs::write(td.path().join("STALE.txt"), "stale\n").unwrap();
    git(td.path(), &["add", "STALE.txt"]);
    git(
        td.path(),
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
    git(td.path(), &["push", "-u", "origin", "feature/stale"]);
    git(td.path(), &["checkout", "main"]);
    git(
        td.path(),
        &["merge", "--no-ff", "feature/stale", "-m", "merge stale"],
    );
    git(td.path(), &["push", "origin", "main"]);
    git(td.path(), &["branch", "-D", "feature/stale"]);
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
    let td = init_repo();
    git(td.path(), &["branch", "-M", "main"]);

    wrt_cmd()
        .current_dir(td.path())
        .arg("housekeeping")
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to clean"));
}

#[test]
fn run_propagates_exit_code_and_requires_separator() {
    let td = init_repo();

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
