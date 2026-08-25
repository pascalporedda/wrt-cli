use anyhow::{Context, Result};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::envx::ResolvedEnvironment;
use crate::gitx::Repo;
use crate::project::{CommandSpec, ProjectConfig};
use crate::state::{Allocation, State};

pub fn run_cmd(dir: &Path, cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("command failed"));
    }
    Ok(())
}

pub fn run_argv_with_wrt_env(
    repo: &Repo,
    state: &State,
    dir: &Path,
    a: &Allocation,
    project: Option<&ProjectConfig>,
    argv: &[String],
) -> Result<i32> {
    let environment = ResolvedEnvironment::build(repo, state, a, project)?;
    run_argv_with_environment(state, dir, a, argv, &environment)
}

fn run_argv_with_environment(
    state: &State,
    dir: &Path,
    a: &Allocation,
    argv: &[String],
    environment: &ResolvedEnvironment,
) -> Result<i32> {
    let cmd = &argv[0];
    let cmd_args = &argv[1..];

    let envs: Vec<(String, String)> = env::vars().collect();

    let mut c = Command::new(cmd);
    c.args(cmd_args)
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit());

    c.env_clear();
    for (k, v) in envs {
        c.env(k, v);
    }
    environment.apply_to(&mut c);
    if Path::new(cmd).file_name().and_then(|name| name.to_str()) == Some("supabase") {
        // Supabase CLI reads `.git/HEAD` to label local database commands. Linked Git
        // worktrees use a `.git` file, so its fallback can report the managed root's branch.
        // GITHUB_HEAD_REF is Supabase's first-choice branch signal.
        let branch = state
            .allocations
            .values()
            .filter(|allocation| dir.starts_with(Path::new(&allocation.path)))
            .max_by_key(|allocation| Path::new(&allocation.path).components().count())
            .map(|allocation| allocation.branch.as_str())
            .unwrap_or(&a.branch);
        c.env("GITHUB_HEAD_REF", branch);
    }

    let status = c.status().with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }
    Ok(0)
}

pub fn run_project_command(
    state: &State,
    worktree_root: &Path,
    allocation: &Allocation,
    environment: &ResolvedEnvironment,
    command: &CommandSpec,
) -> Result<i32> {
    run_argv_with_environment(
        state,
        &command.working_dir(worktree_root),
        allocation,
        command.argv(),
        environment,
    )
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for p in env::split_paths(&path) {
        let cand = p.join(bin);
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

pub fn sh_quote(s: &str) -> String {
    let mut out = String::from("'");
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub fn confirm(prompt: &str) -> Result<bool> {
    use std::io::{self, Write};

    eprint!("{prompt}");
    io::stderr().flush().ok();

    let mut s = String::new();
    io::stdin().read_line(&mut s).context("read user input")?;
    let ans = s.trim().to_lowercase();
    Ok(ans == "y" || ans == "yes")
}

pub fn infer_worktree_from_cwd(st: &State) -> Option<String> {
    let wd = env::current_dir().ok()?;
    let wd = wd.canonicalize().unwrap_or(wd);
    for a in st.allocations.values() {
        let ap = PathBuf::from(&a.path);
        let ap = ap.canonicalize().unwrap_or(ap);
        if wd.strip_prefix(&ap).is_ok() {
            return Some(a.name.clone());
        }
    }
    None
}
