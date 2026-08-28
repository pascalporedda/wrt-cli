use anyhow::Result;
use std::path::Path;

use crate::envx;
use crate::gitx;
use crate::project::ProjectConfig;
use crate::state::{State, StateStore};
use crate::ui;
use crate::util::resolve_worktree_name;
use crate::worktree;

pub fn cmd_path(log: &ui::Logger, st: &State, name: &str) -> Result<i32> {
    let key = worktree::slug(name);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };
    println!("{}", a.path);
    Ok(0)
}

pub fn cmd_env(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    st: &State,
    name: Option<&str>,
) -> Result<i32> {
    let name = resolve_worktree_name(st, name, None);

    let Some(name) = name else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&name);
    let Some(_) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };
    let locked = store.lock_allocation_read(st, &key)?;
    let latest = locked.state();
    let a = locked.allocation(&key);

    let project = ProjectConfig::load_for(&repo.config_root, Path::new(&a.path))?;
    let environment = envx::ResolvedEnvironment::build(repo, latest, a, project.as_ref())?;
    envx::print_exports(&environment);
    Ok(0)
}
