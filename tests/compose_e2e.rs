#[path = "support/compose_project.rs"]
pub mod compose_project;

use compose_project::{ComposeIsolation, ComposeProjectFixture};
use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use wait_timeout::ChildExt;

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
#[test]
#[ignore = "requires docker"]
fn two_worktrees_run_independent_compose_and_source_stacks() {
    require_docker();

    let mut pair = RunningFixturePair::new();
    pair.start("alpha");
    pair.start("beta");
    pair.start_source("alpha");
    pair.start_source("beta");

    let alpha = pair.worktree("alpha");
    let beta = pair.worktree("beta");
    assert_ne!(alpha.compose_project_name(), beta.compose_project_name());
    for key in [
        "POSTGRES_PORT",
        "REDIS_PORT",
        "CORE_API_PORT",
        "WEB_PORT",
        "SOURCE_HTTP_PORT",
    ] {
        assert_ne!(alpha.env[key], beta.env[key], "expected distinct {key}");
    }

    pair.assert_published_ports("alpha");
    pair.assert_published_ports("beta");
    pair.assert_distinct_docker_resources();

    pair.postgres_put("alpha", "alpha");
    pair.postgres_put("beta", "beta");
    pair.redis_put("alpha", "alpha");
    pair.redis_put("beta", "beta");
    pair.assert_markers("alpha", "alpha");
    pair.assert_markers("beta", "beta");
    pair.assert_container_http("alpha");
    pair.assert_container_http("beta");
    pair.assert_source_http("alpha");
    pair.assert_source_http("beta");

    pair.stop("alpha");
    pair.assert_stack_stopped("alpha");
    pair.status_failure("alpha");
    pair.stop("alpha");
    pair.assert_stack_stopped("alpha");
    pair.stop_source("alpha");
    pair.assert_source_stopped("alpha");
    pair.status("beta");
    pair.assert_container_http("beta");
    pair.assert_source_http("beta");
    pair.assert_markers("beta", "beta");

    pair.assert_setup_retry_preserves("beta", "beta");
    pair.stop("beta");
    pair.stop("beta");
    pair.assert_stack_stopped("beta");
    pair.status_failure("beta");
    pair.cleanup_checked();
}

struct RunningFixturePair {
    _fixture: ComposeProjectFixture,
    _managed_root: TempDir,
    root_path: PathBuf,
    worktrees: Vec<RunningFixtureWorktree>,
    source_processes: Vec<SourceProcess>,
    cleaned: bool,
}

struct PendingCommand {
    action: String,
    child: Child,
    stdout: File,
    stderr: File,
}

struct RunningFixtureWorktree {
    name: String,
    path: PathBuf,
    env: BTreeMap<String, String>,
}

struct SourceProcess {
    name: String,
    child: Child,
}

impl RunningFixtureWorktree {
    fn compose_project_name(&self) -> &str {
        &self.env["COMPOSE_PROJECT_NAME"]
    }

    fn port(&self, key: &str) -> u16 {
        self.env[key]
            .parse()
            .unwrap_or_else(|error| panic!("invalid {key} for {}: {error}", self.name))
    }
}

impl RunningFixturePair {
    fn new() -> Self {
        let fixture = ComposeProjectFixture::new(ComposeIsolation::Safe);
        fixture.initialize_repo();
        let managed_root = TempDir::new().expect("create managed root");
        let root_path = managed_root.path().to_path_buf();

        let mut root_init = Command::new(assert_cmd::cargo::cargo_bin!("wrt"));
        root_init.current_dir(fixture.path()).args([
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
        ]);
        assert_success(
            run_bounded(&mut root_init, COMMAND_TIMEOUT),
            "wrt root init",
        );

        let alpha = spawn_new(&root_path, "alpha");
        let beta = spawn_new(&root_path, "beta");
        assert_success(alpha.finish(COMMAND_TIMEOUT), "wrt new alpha");
        assert_success(beta.finish(COMMAND_TIMEOUT), "wrt new beta");

        let worktrees = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                let path = root_path.join(name);
                RunningFixtureWorktree {
                    name: name.to_string(),
                    env: load_wrt_env(&path),
                    path,
                }
            })
            .collect();

        Self {
            _fixture: fixture,
            _managed_root: managed_root,
            root_path,
            worktrees,
            source_processes: Vec::new(),
            cleaned: false,
        }
    }

    fn worktree(&self, name: &str) -> &RunningFixtureWorktree {
        self.worktrees
            .iter()
            .find(|worktree| worktree.name == name)
            .unwrap_or_else(|| panic!("missing worktree {name}"))
    }

    fn setup(&self, name: &str) {
        self.wrt_success(&["setup", name]);
    }

    fn start(&self, name: &str) {
        self.wrt_success(&["runtime", name, "start"]);
        self.wait_until_ready(name);
        self.status(name);
    }

    fn stop(&self, name: &str) {
        self.wrt_success(&["runtime", name, "stop"]);
    }

    fn status(&self, name: &str) {
        self.wrt_success(&["runtime", name, "status"]);
    }

    fn status_failure(&self, name: &str) {
        self.wrt_failure(&["runtime", name, "status"]);
    }

    fn start_source(&mut self, name: &str) {
        let worktree = self.worktree(name);
        let child = Command::new("python3")
            .current_dir(&worktree.path)
            .arg("scripts/source-http.py")
            .envs(&worktree.env)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("start source HTTP process for {name}: {error}"));
        self.source_processes.push(SourceProcess {
            name: name.to_string(),
            child,
        });
        self.wait_for_source(name);
    }

    fn stop_source(&mut self, name: &str) {
        let index = self
            .source_processes
            .iter()
            .position(|process| process.name == name)
            .unwrap_or_else(|| panic!("missing source HTTP process for {name}"));
        let mut process = self.source_processes.swap_remove(index);
        terminate_child(&mut process.child);
    }

    fn wait_for_source(&mut self, name: &str) {
        let port = self.worktree(name).port("SOURCE_HTTP_PORT");
        let process = self
            .source_processes
            .iter_mut()
            .find(|process| process.name == name)
            .unwrap();
        let started = Instant::now();
        while started.elapsed() < READY_TIMEOUT {
            if let Some(status) = process.child.try_wait().expect("poll source HTTP process") {
                let mut stderr = String::new();
                if let Some(mut pipe) = process.child.stderr.take() {
                    pipe.read_to_string(&mut stderr).unwrap();
                }
                panic!("source HTTP process for {name} exited with {status}: {stderr}");
            }
            if http_get(port).is_ok_and(|response| {
                response.contains("200 OK") && response.contains(&format!("source:{name}"))
            }) {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        panic!("timed out waiting for source HTTP process for {name} on port {port}");
    }

    fn assert_source_http(&self, name: &str) {
        let response = http_get(self.worktree(name).port("SOURCE_HTTP_PORT"))
            .unwrap_or_else(|error| panic!("source HTTP request for {name} failed: {error}"));
        assert!(response.contains("200 OK"), "{response}");
        assert!(response.contains(&format!("source:{name}")), "{response}");
    }

    fn assert_source_stopped(&self, name: &str) {
        wait_until_unavailable(
            "source HTTP",
            name,
            self.worktree(name).port("SOURCE_HTTP_PORT"),
        );
    }

    fn postgres_put(&self, name: &str, owner: &str) {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS wrt_markers (marker text primary key, owner text not null); \
             INSERT INTO wrt_markers(marker, owner) VALUES ('shared', '{owner}') \
             ON CONFLICT (marker) DO UPDATE SET owner = EXCLUDED.owner;"
        );
        self.compose_exec(
            name,
            &[
                "exec",
                "-T",
                "postgres",
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                &sql,
            ],
        );
    }

    fn postgres_get(&self, name: &str) -> String {
        self.compose_stdout(
            name,
            &[
                "exec",
                "-T",
                "postgres",
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-At",
                "-c",
                "SELECT owner FROM wrt_markers WHERE marker = 'shared';",
            ],
        )
        .trim()
        .to_string()
    }

    fn redis_put(&self, name: &str, owner: &str) {
        self.compose_exec(
            name,
            &["exec", "-T", "redis", "redis-cli", "SET", "shared", owner],
        );
    }

    fn redis_get(&self, name: &str) -> String {
        self.compose_stdout(name, &["exec", "-T", "redis", "redis-cli", "GET", "shared"])
            .trim()
            .to_string()
    }

    fn assert_markers(&self, name: &str, expected: &str) {
        assert_eq!(self.postgres_get(name), expected);
        assert_eq!(self.redis_get(name), expected);
    }

    fn assert_setup_retry_preserves(&self, name: &str, marker: &str) {
        let before = self.setup_records(name);
        self.setup(name);
        self.setup(name);
        let after = self.setup_records(name);
        assert_eq!(after.len(), before.len() + 2);
        assert_eq!(after[after.len() - 2], after[after.len() - 1]);
        let record = &after[after.len() - 1];
        let worktree = self.worktree(name);
        for key in [
            "COMPOSE_PROJECT_NAME",
            "POSTGRES_PORT",
            "REDIS_PORT",
            "CORE_API_PORT",
            "WEB_PORT",
            "SOURCE_HTTP_PORT",
        ] {
            assert!(
                record.contains(&worktree.env[key]),
                "missing {key} in {record}"
            );
        }
        let command_log = fs::read_to_string(worktree.path.join(".wrt-command.log"))
            .expect("read fixture command log");
        assert!(
            !command_log
                .lines()
                .any(|line| line.starts_with("db:reset\t"))
        );
        self.assert_markers(name, marker);
    }

    fn setup_records(&self, name: &str) -> Vec<String> {
        fs::read_to_string(self.worktree(name).path.join(".wrt-command.log"))
            .expect("read fixture command log")
            .lines()
            .filter(|line| line.starts_with("setup\t"))
            .map(str::to_string)
            .collect()
    }

    fn assert_container_http(&self, name: &str) {
        let worktree = self.worktree(name);
        assert_http_contains(worktree.port("CORE_API_PORT"), &format!("core-api:{name}"));
        assert_http_contains(worktree.port("WEB_PORT"), &format!("web:{name}"));
    }

    fn assert_stack_stopped(&self, name: &str) {
        let output = self.compose_stdout(name, &["ps", "--status", "running", "--quiet"]);
        assert!(
            output.trim().is_empty(),
            "{name} still has running containers: {output}"
        );
        wait_until_unavailable(
            "core API HTTP",
            name,
            self.worktree(name).port("CORE_API_PORT"),
        );
        wait_until_unavailable("web HTTP", name, self.worktree(name).port("WEB_PORT"));
    }

    fn assert_published_ports(&self, name: &str) {
        let worktree = self.worktree(name);
        for (service, target, env_name) in [
            ("postgres", "5432", "POSTGRES_PORT"),
            ("redis", "6379", "REDIS_PORT"),
            ("core-api", "3000", "CORE_API_PORT"),
            ("web", "80", "WEB_PORT"),
        ] {
            let output = self.compose_stdout(name, &["port", service, target]);
            let expected = worktree.port(env_name);
            assert!(
                output
                    .lines()
                    .any(|line| published_port(line) == Some(expected)),
                "{name} {service} does not publish host port {expected}: {output}"
            );
        }
    }

    fn assert_distinct_docker_resources(&self) {
        let alpha = self.worktree("alpha").compose_project_name();
        let beta = self.worktree("beta").compose_project_name();
        let alpha_network = format!("{alpha}_default");
        let beta_network = format!("{beta}_default");
        let alpha_volume = format!("{alpha}_postgres-data");
        let beta_volume = format!("{beta}_postgres-data");
        assert_ne!(alpha_network, beta_network);
        assert_ne!(alpha_volume, beta_volume);
        assert_ne!(
            docker_resource_field("network", &alpha_network, ".Id"),
            docker_resource_field("network", &beta_network, ".Id")
        );
        assert_eq!(
            docker_resource_field("volume", &alpha_volume, ".Name"),
            alpha_volume
        );
        assert_eq!(
            docker_resource_field("volume", &beta_volume, ".Name"),
            beta_volume
        );
    }

    fn wait_until_ready(&self, name: &str) {
        let worktree = self.worktree(name);
        wait_for("Postgres readiness", name, || {
            self.compose_ok(
                name,
                &[
                    "exec",
                    "-T",
                    "postgres",
                    "pg_isready",
                    "-U",
                    "postgres",
                    "-d",
                    "postgres",
                ],
            )
        });
        wait_for("Redis readiness", name, || {
            self.compose_try_stdout(name, &["exec", "-T", "redis", "redis-cli", "PING"])
                .is_some_and(|output| output.trim() == "PONG")
        });
        wait_for("core API HTTP", name, || {
            http_ok(worktree.port("CORE_API_PORT"))
        });
        wait_for("web HTTP", name, || http_ok(worktree.port("WEB_PORT")));
    }

    fn wrt_success(&self, args: &[&str]) {
        let (output, action) = self.wrt_output(args);
        assert_success(output, &action);
    }

    fn wrt_failure(&self, args: &[&str]) {
        let (output, action) = self.wrt_output(args);
        assert!(!output.status.success(), "{action} unexpectedly succeeded");
    }

    fn wrt_output(&self, args: &[&str]) -> (Output, String) {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("wrt"));
        command.current_dir(&self.root_path).args(args);
        (
            run_bounded(&mut command, COMMAND_TIMEOUT),
            format!("wrt {}", args.join(" ")),
        )
    }

    fn compose_exec(&self, name: &str, args: &[&str]) {
        let output = self.compose_output(name, args);
        assert_success(output, &format!("docker compose {}", args.join(" ")));
    }

    fn compose_stdout(&self, name: &str, args: &[&str]) -> String {
        let output = self.compose_output(name, args);
        assert_success(output, &format!("docker compose {}", args.join(" ")))
    }

    fn compose_output(&self, name: &str, args: &[&str]) -> Output {
        run_bounded(
            &mut compose_command(self.worktree(name), args),
            COMMAND_TIMEOUT,
        )
    }

    fn compose_try_stdout(&self, name: &str, args: &[&str]) -> Option<String> {
        try_run_bounded(
            &mut compose_command(self.worktree(name), args),
            Duration::from_secs(5),
        )
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn compose_ok(&self, name: &str, args: &[&str]) -> bool {
        try_run_bounded(
            &mut compose_command(self.worktree(name), args),
            Duration::from_secs(5),
        )
        .is_some_and(|output| output.status.success())
    }

    fn cleanup_checked(&mut self) {
        for process in &mut self.source_processes {
            terminate_child(&mut process.child);
        }
        self.source_processes.clear();
        for worktree in &self.worktrees {
            wait_until_unavailable(
                "source HTTP",
                &worktree.name,
                worktree.port("SOURCE_HTTP_PORT"),
            );
        }
        for worktree in &self.worktrees {
            let output = run_bounded(
                &mut compose_command(worktree, &["down", "--volumes", "--remove-orphans"]),
                CLEANUP_TIMEOUT,
            );
            assert_success(output, &format!("clean up {}", worktree.name));
            assert_scoped_resources_absent(worktree);
        }
        self.cleaned = true;
    }

    fn cleanup_best_effort(&mut self) {
        for process in &mut self.source_processes {
            terminate_child(&mut process.child);
        }
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        for worktree in &self.worktrees {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                eprintln!("cleanup deadline expired before {}", worktree.name);
                break;
            }
            let action = format!("clean up {}", worktree.name);
            match try_run_bounded(
                &mut compose_command(worktree, &["down", "--volumes", "--remove-orphans"]),
                remaining,
            ) {
                Some(output) if output.status.success() => {}
                Some(output) => eprintln!(
                    "{action} failed with {:?}: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ),
                None => eprintln!("{action} timed out or could not start"),
            }
        }
    }
}

impl Drop for RunningFixturePair {
    fn drop(&mut self) {
        if !self.cleaned {
            self.cleanup_best_effort();
        }
    }
}

fn spawn_new(root: &Path, name: &str) -> PendingCommand {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("wrt"));
    command.current_dir(root).args([
        "new",
        name,
        "--install",
        "false",
        "--supabase",
        "none",
        "--db",
        "false",
    ]);
    PendingCommand::spawn(&mut command, &format!("wrt new {name}"))
}

fn compose_command(worktree: &RunningFixtureWorktree, args: &[&str]) -> Command {
    let mut command = Command::new("docker");
    command
        .current_dir(&worktree.path)
        .args(["compose"])
        .args(args)
        .envs(&worktree.env);
    command
}

impl PendingCommand {
    fn spawn(command: &mut Command, action: &str) -> Self {
        Self::try_spawn(command, action)
            .unwrap_or_else(|| panic!("start timed command for {action}"))
    }

    fn try_spawn(command: &mut Command, action: &str) -> Option<Self> {
        let stdout = tempfile::tempfile().ok()?;
        let stderr = tempfile::tempfile().ok()?;
        let child = command
            .stdout(Stdio::from(stdout.try_clone().ok()?))
            .stderr(Stdio::from(stderr.try_clone().ok()?))
            .spawn()
            .ok()?;
        Some(Self {
            action: action.to_string(),
            child,
            stdout,
            stderr,
        })
    }

    fn finish(mut self, timeout: Duration) -> Output {
        let action = self.action.clone();
        self.try_finish(timeout)
            .unwrap_or_else(|| panic!("{action} timed out after {} seconds", timeout.as_secs()))
    }

    fn try_finish(&mut self, timeout: Duration) -> Option<Output> {
        let status = match self.child.wait_timeout(timeout) {
            Ok(Some(status)) => status,
            Ok(None) | Err(_) => {
                terminate_child(&mut self.child);
                return None;
            }
        };
        Some(Output {
            status,
            stdout: read_output(&mut self.stdout)?,
            stderr: read_output(&mut self.stderr)?,
        })
    }
}

fn run_bounded(command: &mut Command, timeout: Duration) -> Output {
    let action = format!("{command:?}");
    PendingCommand::spawn(command, &action).finish(timeout)
}

fn try_run_bounded(command: &mut Command, timeout: Duration) -> Option<Output> {
    let action = format!("{command:?}");
    let mut pending = PendingCommand::try_spawn(command, &action)?;
    pending.try_finish(timeout)
}

fn read_output(file: &mut File) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn assert_scoped_resources_absent(worktree: &RunningFixtureWorktree) {
    let output = run_bounded(
        &mut compose_command(worktree, &["ps", "--all", "--quiet"]),
        CLEANUP_TIMEOUT,
    );
    let containers = assert_success(output, &format!("inspect {} containers", worktree.name));
    assert!(
        containers.trim().is_empty(),
        "{} containers remain: {containers}",
        worktree.name
    );
    let project = worktree.compose_project_name();
    assert!(!docker_object_exists(
        "network",
        &format!("{project}_default")
    ));
    assert!(!docker_object_exists(
        "volume",
        &format!("{project}_postgres-data")
    ));
}

fn docker_object_exists(kind: &str, name: &str) -> bool {
    let mut command = Command::new("docker");
    command.args([kind, "inspect", name]);
    run_bounded(&mut command, CLEANUP_TIMEOUT).status.success()
}

fn load_wrt_env(worktree_path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(worktree_path.join(".wrt.env"))
        .expect("read .wrt.env")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (key, value) = line.split_once('=').expect("split env line");
            (
                key.to_string(),
                value.trim().trim_matches('\'').replace("'\\''", "'"),
            )
        })
        .collect()
}

fn wait_for(label: &str, name: &str, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < READY_TIMEOUT {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
    panic!("timed out waiting for {label} in {name}");
}

fn http_ok(port: u16) -> bool {
    http_get(port).is_ok_and(|response| response.contains("200 OK"))
}

fn assert_http_contains(port: u16, expected: &str) {
    let response =
        http_get(port).unwrap_or_else(|error| panic!("HTTP request failed on {port}: {error}"));
    assert!(
        response.contains("200 OK"),
        "unexpected response on {port}: {response}"
    );
    assert!(response.contains(expected), "{response}");
}

fn wait_until_unavailable(label: &str, name: &str, port: u16) {
    let started = Instant::now();
    while started.elapsed() < READY_TIMEOUT {
        if http_get(port).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("{label} for {name} still answers on port {port}");
}

fn http_get(port: u16) -> std::io::Result<String> {
    let address = ("127.0.0.1", port)
        .to_socket_addrs()?
        .next()
        .expect("resolve loopback address");
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn published_port(line: &str) -> Option<u16> {
    line.trim().rsplit_once(':')?.1.parse().ok()
}

fn docker_resource_field(kind: &str, name: &str, field: &str) -> String {
    let template = format!("{{{{{field}}}}}");
    let mut command = Command::new("docker");
    command.args([kind, "inspect", "--format", &template, name]);
    let output = run_bounded(&mut command, COMMAND_TIMEOUT);
    assert_success(output, &format!("docker {kind} inspect {name}"))
        .trim()
        .to_string()
}

fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn assert_success(output: Output, action: &str) -> String {
    if !output.status.success() {
        panic!(
            "{action} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

fn require_docker() {
    let mut compose = Command::new("docker");
    compose.args(["compose", "version"]);
    let output = run_bounded(&mut compose, Duration::from_secs(10));
    assert_success(output, "docker compose version");
    let mut info = Command::new("docker");
    info.args(["info", "--format", "{{.ServerVersion}}"]);
    let output = run_bounded(&mut info, Duration::from_secs(10));
    assert_success(output, "docker info");
}
