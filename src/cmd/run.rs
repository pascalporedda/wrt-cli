use anyhow::Result;
use std::env;
use std::process::{Command, Stdio};

use crate::state::State;
use crate::ui;
use crate::worktree;

pub fn raw_run_has_sep(raw_args: &[String]) -> bool {
    if raw_args.len() < 4 {
        return false;
    }
    if raw_args.get(1).map(|s| s.as_str()) != Some("run") {
        return true;
    }
    match raw_args.iter().position(|s| s == "--") {
        Some(i) => i == 3,
        None => false,
    }
}

pub fn cmd_run(log: &ui::Logger, st: &State, name: &str, command: &[String]) -> Result<i32> {
    if command.is_empty() {
        log.errorf("usage: wrt run <name> -- <command> [args...]");
        return Ok(2);
    }

    let key = worktree::slug(name);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    let cmd = &command[0];
    let cmd_args = &command[1..];

    let mut envs: Vec<(String, String)> = env::vars().collect();
    envs.push(("WRT_NAME".into(), a.name.clone()));
    envs.push(("WRT_BRANCH".into(), a.branch.clone()));
    envs.push(("WRT_PORT_BLOCK".into(), a.block.to_string()));
    envs.push(("WRT_PORT_OFFSET".into(), a.offset.to_string()));

    log.infof(&format!(
        "run: {cmd} {} (in {})",
        cmd_args.join(" "),
        a.path
    ));

    let mut c = Command::new(cmd);
    c.args(cmd_args)
        .current_dir(&a.path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit());

    c.env_clear();
    for (k, v) in envs {
        c.env(k, v);
    }

    let status = match c.status() {
        Ok(s) => s,
        Err(e) => {
            log.errorf(&format!("run failed: {e}"));
            return Ok(1);
        }
    };

    Ok(status.code().unwrap_or(1))
}
