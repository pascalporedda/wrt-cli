use anyhow::Result;

use crate::envx;
use crate::gitx;
use crate::state::State;
use crate::ui;
use crate::util::infer_worktree_from_cwd;
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

pub fn cmd_env(log: &ui::Logger, repo: &gitx::Repo, st: &State, name: Option<&str>) -> Result<i32> {
    let mut name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    if name.is_none() {
        name = infer_worktree_from_cwd(st);
    }

    let Some(name) = name else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&name);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    envx::print_exports(repo, a);
    Ok(0)
}
