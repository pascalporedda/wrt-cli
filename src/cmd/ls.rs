use anyhow::Result;
use std::path::Path;

use crate::state::State;
use crate::worktree;

pub fn cmd_ls(st: &State) -> Result<i32> {
    if st.allocations.is_empty() {
        println!("(no worktrees tracked by wrt)");
        return Ok(0);
    }

    for a in st.sorted_allocations() {
        let dirty = match worktree::is_dirty(Path::new(&a.path)) {
            Ok(true) => "dirty",
            Ok(false) => "clean",
            Err(_) => "?",
        };
        println!(
            "{:<28}  block={:<3}  offset={:<4}  {:<5}  {}  ({})",
            a.name, a.block, a.offset, dirty, a.branch, a.path
        );
    }

    Ok(0)
}
