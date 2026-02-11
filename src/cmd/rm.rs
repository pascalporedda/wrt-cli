use anyhow::Result;
use std::path::Path;

use crate::gitx;
use crate::state::State;
use crate::supabase;
use crate::ui;
use crate::util::{run_cmd, which};
use crate::worktree;

pub fn cmd_rm(
    log: &ui::Logger,
    repo: &gitx::Repo,
    st: &mut State,
    name: &str,
    force: bool,
    delete_branch: bool,
) -> Result<i32> {
    let key = worktree::slug(name);
    let Some(a) = st.allocations.get(&key).cloned() else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    log.infof(&format!("removing worktree: {} ({})", a.name, a.path));

    let wt_path = Path::new(&a.path);
    if wt_path.exists() && supabase::has_config(wt_path) && which("supabase").is_some() {
        log.infof("stopping supabase containers");
        if let Err(e) = run_cmd(wt_path, "supabase", &["stop"]) {
            log.errorf(&format!("supabase stop failed: {e}"));
            if !force {
                return Ok(1);
            }
            log.infof("continuing anyway (--force)");
        }
    }

    if let Err(e) = worktree::remove(&repo.root, wt_path, force) {
        log.errorf(&format!("git worktree remove failed: {e}"));
        return Ok(1);
    }

    if delete_branch {
        log.infof(&format!("deleting branch: {}", a.branch));
        if let Err(e) = run_cmd(&repo.root, "git", &["branch", "-D", &a.branch]) {
            log.errorf(&format!("branch delete failed: {e}"));
            return Ok(1);
        }
    }

    st.allocations.remove(&key);
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    Ok(0)
}
