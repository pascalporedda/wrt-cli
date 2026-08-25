use anyhow::Result;
use std::path::Path;

use crate::gitx;
use crate::state::{State, StateStore};
use crate::ui;
use crate::util::run_cmd;

pub fn cmd_prune(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    st: &mut State,
) -> Result<i32> {
    log.infof("git worktree prune");
    if let Err(e) = run_cmd(&repo.root, "git", &["worktree", "prune"]) {
        log.errorf(&format!("prune failed: {e}"));
        return Ok(1);
    }

    let removed = match store.update(|state| {
        let keys = state.allocations.keys().cloned().collect::<Vec<_>>();
        let mut removed = 0;
        for key in keys {
            let Some(allocation) = state.allocations.get(&key) else {
                continue;
            };
            if !Path::new(&allocation.path).exists() && !state.is_primary_allocation(allocation) {
                state.allocations.remove(&key);
                removed += 1;
            }
        }
        Ok(removed)
    }) {
        Ok(removed) => removed,
        Err(error) => {
            log.errorf(&format!("state save failed: {error}"));
            return Ok(1);
        }
    };
    *st = store.read()?;
    if removed > 0 {
        log.infof(&format!("state: removed {removed} missing worktrees"));
    }

    Ok(0)
}
