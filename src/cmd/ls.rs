use anyhow::Result;
use std::path::Path;

use crate::state::{State, SupabaseAllocation};
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
        let supabase = match &a.supabase {
            SupabaseAllocation::Owned { .. } if st.is_primary_allocation(&a) => {
                "supabase=owner(main)"
            }
            SupabaseAllocation::Owned { .. } => "supabase=isolated",
            SupabaseAllocation::Shared { owner } => {
                if owner == "main" || st.primary_allocation_key() == Some(owner.as_str()) {
                    "supabase=shared(main)"
                } else {
                    "supabase=shared"
                }
            }
            SupabaseAllocation::None => "supabase=none",
        };
        println!(
            "{:<28}  block={:<3}  offset={:<4}  {:<8}  {:<5}  {:<17}  {}  ({})",
            a.name, a.block, a.offset, a.status, dirty, supabase, a.branch, a.path
        );
    }

    Ok(0)
}
