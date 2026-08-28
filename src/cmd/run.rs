use anyhow::Result;
use std::path::Path;

use crate::gitx;
use crate::project::ProjectConfig;
use crate::state::{State, StateStore};
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

pub fn cmd_run(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    st: &State,
    name: &str,
    command: &[String],
) -> Result<i32> {
    if command.is_empty() {
        log.errorf("usage: wrt run <name> -- <command> [args...]");
        return Ok(2);
    }

    let key = worktree::slug(name);
    let Some(_) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };
    let locked = store.lock_allocation_read(st, &key)?;
    let latest = locked.state();
    let a = locked.allocation(&key);

    log.infof(&format!(
        "run: {} {} (in {})",
        command[0],
        command[1..].join(" "),
        a.path
    ));

    let project = ProjectConfig::load_for(&repo.config_root, Path::new(&a.path))?;
    match run_argv_with_wrt_env(
        repo,
        latest,
        Path::new(&a.path),
        a,
        project.as_ref(),
        command,
    ) {
        Ok(code) => Ok(code),
        Err(e) => {
            log.errorf(&format!("run failed: {e}"));
            Ok(1)
        }
    }
}
