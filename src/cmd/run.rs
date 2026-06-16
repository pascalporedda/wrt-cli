use anyhow::Result;
use std::path::Path;

use crate::state::State;
use crate::ui;
use crate::util::run_argv_with_wrt_env;
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

    log.infof(&format!(
        "run: {} {} (in {})",
        command[0],
        command[1..].join(" "),
        a.path
    ));

    match run_argv_with_wrt_env(Path::new(&a.path), a, command) {
        Ok(code) => Ok(code),
        Err(e) => {
            log.errorf(&format!("run failed: {e}"));
            Ok(1)
        }
    }
}
