use anyhow::Result;
use std::path::Path;

use crate::gitx;
use crate::state::State;
use crate::ui;
use crate::util::run_cmd;

pub fn cmd_prune(log: &ui::Logger, repo: &gitx::Repo, st: &mut State) -> Result<i32> {
    log.infof("git worktree prune");
    if let Err(e) = run_cmd(&repo.root, "git", &["worktree", "prune"]) {
        log.errorf(&format!("prune failed: {e}"));
        return Ok(1);
    }

    let mut removed = 0;
    let keys: Vec<String> = st.allocations.keys().cloned().collect();
    for k in keys {
        let missing = st
            .allocations
            .get(&k)
            .map(|a| !Path::new(&a.path).exists())
            .unwrap_or(false);
        if missing {
            st.allocations.remove(&k);
            removed += 1;
        }
    }

    if removed > 0 {
        log.infof(&format!("state: removed {removed} missing worktrees"));
        if let Err(e) = st.save(&repo.common_dir) {
            log.errorf(&format!("state save failed: {e}"));
            return Ok(1);
        }
    }

    Ok(0)
}
