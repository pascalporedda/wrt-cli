use anyhow::Result;
use std::io::IsTerminal;
use std::path::Path;

use crate::gitx;
use crate::state::{State, SupabaseAllocation};
use crate::supabase;
use crate::ui;
use crate::util::{confirm, which};
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
    if st.is_primary_allocation(&a) {
        log.errorf("the main worktree cannot be removed");
        return Ok(2);
    }

    let interactive = std::io::stdin().is_terminal();
    let upstream = if delete_branch || interactive {
        worktree::branch_upstream(&repo.common_dir, &a.branch)?
    } else {
        None
    };
    let delete_branch = delete_branch
        || (interactive && confirm(&branch_delete_prompt(&a.branch, upstream.as_ref()))?);

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

    if worktree::is_registered(&repo.common_dir, wt_path)? {
        if let Err(e) = worktree::remove(&repo.common_dir, wt_path, force) {
            if worktree::is_registered(&repo.common_dir, wt_path)? {
                log.errorf(&format!("git worktree remove failed: {e}"));
                return Ok(1);
            }
            log.infof(&format!(
                "git reported a removal error after deregistering the worktree; continuing cleanup: {e}"
            ));
        }
    } else {
        log.infof("worktree is already deregistered; continuing cleanup");
    }

    let mut worktree_cleanup_failed = false;
    if wt_path.symlink_metadata().is_ok() {
        log.infof(&format!(
            "removing leftover worktree path: {}",
            wt_path.display()
        ));
        if let Err(e) = worktree::remove_residual_path(wt_path) {
            log.errorf(&format!("leftover worktree path removal failed: {e}"));
            worktree_cleanup_failed = true;
        }
    }

    let mut branch_cleanup_failed = false;
    if delete_branch {
        if let Some(upstream) = &upstream {
            log.infof(&format!(
                "deleting remote branch: {}/{}",
                upstream.remote, upstream.branch
            ));
            if let Err(e) =
                worktree::delete_remote_branch(&repo.common_dir, &upstream.remote, &upstream.branch)
            {
                log.errorf(&format!("remote branch delete failed: {e}"));
                branch_cleanup_failed = true;
            }
        }

        if !branch_cleanup_failed {
            log.infof(&format!("deleting branch: {}", a.branch));
            if let Err(e) = worktree::delete_branch(&repo.common_dir, &a.branch) {
                log.errorf(&format!("branch delete failed: {e}"));
                branch_cleanup_failed = true;
            }
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

    Ok(i32::from(worktree_cleanup_failed || branch_cleanup_failed))
}

fn branch_delete_prompt(branch: &str, upstream: Option<&worktree::UpstreamBranch>) -> String {
    match upstream {
        Some(upstream) => format!(
            "Also delete local branch {branch} and remote branch {}/{}? (y/N): ",
            upstream.remote, upstream.branch
        ),
        None => format!("Also delete local branch {branch}? (y/N): "),
    }
}
