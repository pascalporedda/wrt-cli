use anyhow::Result;
use std::path::Path;

use crate::gitx;
use crate::state::{AllocationGeneration, State, StateStore};
use crate::ui;
use crate::worktree;

pub fn cmd_prune(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    st: &mut State,
) -> Result<i32> {
    log.infof("git worktree prune");
    if let Err(e) = worktree::prune(&repo.common_dir) {
        log.errorf(&format!("prune failed: {e}"));
        return Ok(1);
    }

    let candidates = st
        .allocations
        .iter()
        .filter(|(_, allocation)| {
            !Path::new(&allocation.path).exists() && !st.is_primary_allocation(allocation)
        })
        .map(|(key, allocation)| (key.clone(), allocation.generation_id))
        .collect::<Vec<_>>();
    let mut removed = 0;
    for (key, generation) in candidates {
        let did_remove = match prune_candidate(store, &key, generation) {
            Ok(removed) => removed,
            Err(error) => {
                log.errorf(&format!("state save failed: {error}"));
                return Ok(1);
            }
        };
        removed += usize::from(did_remove);
    }
    *st = store.read()?;
    if removed > 0 {
        log.infof(&format!("state: removed {removed} missing worktrees"));
    }

    Ok(0)
}

fn prune_candidate(
    store: &StateStore,
    key: &str,
    generation: AllocationGeneration,
) -> Result<bool> {
    let _lifecycle = store.lock_allocation(key)?;
    store.update(|state| {
        let Some(allocation) = state.allocations.get(key) else {
            return Ok(false);
        };
        if allocation.generation_id != generation
            || state.is_primary_allocation(allocation)
            || Path::new(&allocation.path).exists()
        {
            return Ok(false);
        }
        state.remove_if_generation(key, generation)?;
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        Allocation, AllocationSetup, LAYOUT_MANAGED_ROOT, PortAssignments, RootState,
        SupabaseAllocation,
    };
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn allocation(name: &str, path: &Path, block: i32) -> Allocation {
        Allocation {
            generation_id: AllocationGeneration::new(),
            name: name.to_string(),
            branch: name.to_string(),
            path: path.to_string_lossy().to_string(),
            block,
            offset: block * 100,
            ports: PortAssignments::new(),
            status: "active".to_string(),
            created_at: "now".to_string(),
            supabase: SupabaseAllocation::None,
            setup: AllocationSetup::default(),
        }
    }

    #[test]
    fn prune_rechecks_path_after_waiting_for_the_lifecycle_lock() {
        let temp = tempfile::tempdir().unwrap();
        let common = temp.path().join(".git");
        let main = temp.path().join("main");
        let feature = temp.path().join("feature");
        fs::create_dir_all(&common).unwrap();
        fs::create_dir_all(&main).unwrap();
        let repo = gitx::Repo::new(
            temp.path().to_path_buf(),
            common.clone(),
            main.clone(),
            temp.path().to_path_buf(),
            Some(main.clone()),
        );
        let store = StateStore::new(&repo);
        let feature_allocation = allocation("feature", &feature, 1);
        let generation = feature_allocation.generation_id;
        store
            .update(|state| {
                state.root = Some(RootState {
                    layout: LAYOUT_MANAGED_ROOT.to_string(),
                    managed_root: temp.path().to_string_lossy().to_string(),
                    git_common_dir: common.to_string_lossy().to_string(),
                    main_worktree: main.to_string_lossy().to_string(),
                    worktrees_path: temp.path().to_string_lossy().to_string(),
                    created_at: "now".to_string(),
                    supabase_config_path: None,
                });
                state
                    .allocations
                    .insert("main".to_string(), allocation("main", &main, 0));
                state
                    .allocations
                    .insert("feature".to_string(), feature_allocation);
                Ok(())
            })
            .unwrap();

        let held = store.lock_allocation("feature").unwrap();
        let waiting_store = store.clone();
        let (sender, receiver) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let result = prune_candidate(&waiting_store, "feature", generation).unwrap();
            sender.send(result).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        fs::create_dir(&feature).unwrap();
        drop(held);

        assert!(!receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        waiter.join().unwrap();
        assert!(store.read().unwrap().allocations.contains_key("feature"));
    }
}
