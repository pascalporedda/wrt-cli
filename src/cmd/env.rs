use anyhow::Result;

use crate::state::State;
use crate::ui;
use crate::util::{infer_worktree_from_cwd, sh_quote};
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

pub fn cmd_env(log: &ui::Logger, st: &State, name: Option<&str>) -> Result<i32> {
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

    println!("export WRT_NAME={}", sh_quote(&a.name));
    println!("export WRT_BRANCH={}", sh_quote(&a.branch));
    println!("export WRT_PORT_BLOCK={}", a.block);
    println!("export WRT_PORT_OFFSET={}", a.offset);
    Ok(0)
}
