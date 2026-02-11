use anyhow::{Context, Result};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

pub fn run_argv_with_wrt_env(dir: &Path, a: &Allocation, argv: &[String]) -> Result<()> {
    let cmd = &argv[0];
    let cmd_args = &argv[1..];

    let mut envs: Vec<(String, String)> = env::vars().collect();
    envs.push(("WRT_NAME".into(), a.name.clone()));
    envs.push(("WRT_BRANCH".into(), a.branch.clone()));
    envs.push(("WRT_PORT_BLOCK".into(), a.block.to_string()));
    envs.push(("WRT_PORT_OFFSET".into(), a.offset.to_string()));

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

    let status = c.status().with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("command failed"));
    }
    Ok(())
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
    for a in st.allocations.values() {
        let ap = PathBuf::from(&a.path);
        if wd.strip_prefix(&ap).is_ok() {
            return Some(a.name.clone());
        }
    }
    None
}
