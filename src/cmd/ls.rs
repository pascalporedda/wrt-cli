use anyhow::Result;
use std::path::Path;

use crate::state::State;
use crate::supabase;
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
        let supabase = if supabase::has_config(Path::new(&a.path)) {
            "supabase=patched"
        } else {
            "supabase=none"
        };
        println!(
            "{:<28}  block={:<3}  offset={:<4}  {:<8}  {:<5}  {:<17}  {}  ({})",
            a.name, a.block, a.offset, a.status, dirty, supabase, a.branch, a.path
        );
    }

    Ok(0)
}
