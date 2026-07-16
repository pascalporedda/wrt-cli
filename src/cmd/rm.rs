use anyhow::Result;
use std::path::Path;

use crate::gitx;
use crate::state::{State, SupabaseAllocation};
use crate::supabase;
use crate::ui;
use crate::util::which;
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
    if key == "main" {
        log.errorf("the main worktree cannot be removed");
        return Ok(2);
    }
    let Some(a) = st.allocations.get(&key).cloned() else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    log.infof(&format!("removing worktree: {} ({})", a.name, a.path));

    let wt_path = Path::new(&a.path);
    let owned = match &a.supabase {
        SupabaseAllocation::Owned {
            project_id,
            config_path,
        } => Some((
            project_id.as_str(),
            supabase::Target::from_config_path(config_path)?,
        )),
        SupabaseAllocation::None | SupabaseAllocation::Shared { .. } => None,
    };
    let mut cleanup_hint = None;
    if let Some((project_id, target)) = owned.as_ref().filter(|_| wt_path.exists()) {
        if which("supabase").is_none() {
            log.errorf("cannot stop owned Supabase stack: CLI not found in PATH");
            if !force {
                return Ok(1);
            }
            cleanup_hint = Some(*project_id);
        } else {
            log.infof("stopping supabase containers");
            if let Err(e) = supabase::stop(wt_path, target) {
                log.errorf(&format!("supabase stop failed: {e}"));
                if !force {
                    return Ok(1);
                }
                cleanup_hint = Some(*project_id);
            }
        }
    }

    if let Err(e) = worktree::remove(&repo.common_dir, wt_path, force) {
        log.errorf(&format!("git worktree remove failed: {e}"));
        return Ok(1);
    }

    if delete_branch {
        log.infof(&format!("deleting branch: {}", a.branch));
        if let Err(e) = worktree::delete_branch(&repo.common_dir, &a.branch) {
            log.errorf(&format!("branch delete failed: {e}"));
            return Ok(1);
        }
    }

    if let Some(project_id) = cleanup_hint {
        log.errorf(&format!(
            "Supabase cleanup is still required: supabase stop --project-id {project_id}"
        ));
    }
    st.allocations.remove(&key);
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    Ok(0)
}
