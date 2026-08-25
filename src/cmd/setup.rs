use anyhow::Result;
use std::path::Path;

use crate::cmd::new::{SetupModes, setup_existing_worktree, update_status};
use crate::gitx;
use crate::project::ProjectConfig;
use crate::state::{State, StateStore};
use crate::ui;
use crate::worktree;

pub fn cmd_setup(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    state: &mut State,
    name: &str,
) -> Result<i32> {
    let key = worktree::slug(name);
    let allocation = match state.allocations.get(&key) {
        Some(allocation) => allocation.clone(),
        None => {
            log.errorf(&format!("unknown worktree: \"{key}\""));
            return Ok(2);
        }
    };
    let worktree_path = Path::new(&allocation.path);
    if !worktree_path.is_dir() {
        log.errorf(&format!(
            "worktree path is missing: {}",
            worktree_path.display()
        ));
        return Ok(2);
    }
    let project = ProjectConfig::load_for(&repo.config_root, worktree_path)?;
    let modes = SetupModes {
        install_mode: &allocation.setup.install,
        db_mode: &allocation.setup.db,
    };
    if let Err(error) = setup_existing_worktree(
        log,
        repo,
        store,
        state,
        &allocation,
        project.as_ref(),
        modes,
    ) {
        if let Err(status_error) = update_status(store, &key, allocation.generation_id, "failed") {
            log.errorf(&format!("record setup failure failed: {status_error}"));
        }
        log.errorf(&format!("setup failed: {error}"));
        return Ok(1);
    }
    update_status(store, &key, allocation.generation_id, "active")?;
    *state = store.read()?;
    Ok(0)
}
