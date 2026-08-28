use anyhow::Result;
use chrono::SecondsFormat;
use std::io::IsTerminal;
use std::path::Path;

use crate::cli::FeatureSupabaseMode;
use crate::cmd::setup::{SetupModes, setup_existing_worktree, update_status};
use crate::envx;
use crate::gitx;
use crate::project::ProjectConfig;
use crate::state::{
    Allocation, AllocationGeneration, AllocationSetup, PortAssignments, ReservationRequest, State,
    StateStore, SupabaseAllocation, merge_port_assignments, reserve_ports,
};
use crate::supabase;
use crate::ui;
use crate::util::{confirm, sh_quote};
use crate::worktree;

pub struct NewOpts<'a> {
    pub name: &'a str,
    pub from_ref: &'a str,
    pub branch: Option<&'a str>,
    pub install_mode: &'a str,
    pub sb_mode: FeatureSupabaseMode,
    pub supabase_config: Option<&'a str>,
    pub db_mode: &'a str,
    pub emit_cd: bool,
}

enum ResolvedSupabaseMode {
    Shared,
    Isolated,
    None,
}

pub fn cmd_new(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    st: &mut State,
    opts: NewOpts<'_>,
) -> Result<i32> {
    let wt_name = worktree::slug(opts.name);
    let wt_path = repo.worktree_path(&wt_name);

    let mut br = opts.branch.unwrap_or("").trim().to_string();
    if br.is_empty() {
        br = opts.name.to_string();
    }
    br = worktree::normalize_branch(&br);

    if st.allocations.contains_key(&wt_name) {
        log.errorf(&format!(
            "worktree \"{wt_name}\" already exists in state; use `wrt ls`"
        ));
        return Ok(2);
    }

    log.infof(&format!(
        "creating worktree: {wt_name} ({br}) at {}",
        wt_path.display()
    ));

    worktree::ensure_dir(wt_path.parent().unwrap())?;

    if let Err(e) = worktree::add(&repo.common_dir, &wt_path, &br, opts.from_ref) {
        log.errorf(&format!("git worktree add failed: {e}"));
        return Ok(1);
    }

    let project_config = match ProjectConfig::load_for(&repo.config_root, &wt_path) {
        Ok(config) => config,
        Err(error) => {
            log.errorf(&format!("project config failed: {error}"));
            remove_failed_worktree(log, repo, &wt_path);
            return Ok(2);
        }
    };
    let supabase = match resolve_feature_supabase(
        log,
        repo,
        store,
        st,
        &wt_path,
        project_config.as_ref(),
        &opts,
    ) {
        Ok(supabase) => supabase,
        Err(error) => {
            log.errorf(&format!("Supabase setup selection failed: {error}"));
            remove_failed_worktree(log, repo, &wt_path);
            return Ok(2);
        }
    };
    let supabase_base_claims = match isolated_supabase_port_claims(&wt_path, &supabase) {
        Ok(claims) => claims,
        Err(error) => {
            log.errorf(&format!("Supabase port discovery failed: {error}"));
            remove_failed_worktree(log, repo, &wt_path);
            return Ok(2);
        }
    };
    let reservation_request =
        ReservationRequest::new(project_config.as_ref(), supabase_base_claims);
    let created_at = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let generation_id = AllocationGeneration::new();
    let reserved = store.update(|state| {
        if state.allocations.contains_key(&wt_name) {
            anyhow::bail!("worktree {wt_name:?} already exists in state; use `wrt ls`");
        }
        let reservation = reserve_ports(state, &reservation_request)?;
        let allocation = Allocation {
            generation_id,
            name: wt_name.clone(),
            branch: br.clone(),
            path: wt_path.to_string_lossy().to_string(),
            block: reservation.block,
            offset: reservation.offset,
            ports: reservation.ports,
            status: "creating".to_string(),
            created_at,
            supabase,
            setup: AllocationSetup {
                install: opts.install_mode.to_string(),
                db: opts.db_mode.to_string(),
            },
        };
        state.allocations.insert(wt_name.clone(), allocation);
        Ok(())
    });
    if let Err(error) = reserved {
        log.errorf(&format!("reserve allocation failed: {error:#}"));
        if let Err(cleanup_error) = clear_failed_reservation(store, &wt_name, generation_id) {
            log.errorf(&format!(
                "reservation cleanup failed; keeping worktree {}: {cleanup_error:#}",
                wt_path.display()
            ));
            return Ok(1);
        }
        remove_failed_worktree(log, repo, &wt_path);
        return Ok(1);
    }
    *st = store.read()?;

    let setup = SetupModes {
        install_mode: opts.install_mode,
        db_mode: opts.db_mode,
    };
    let setup_allocation = st
        .allocations
        .get(&wt_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("reserved allocation is missing: {wt_name}"))?;
    if let Err(e) = setup_existing_worktree(
        log,
        repo,
        store,
        st,
        &setup_allocation,
        project_config.as_ref(),
        setup,
    ) {
        if let Err(status_error) = update_status(store, &wt_name, generation_id, "failed") {
            log.errorf(&format!("record setup failure failed: {status_error}"));
        }
        log.errorf(&format!("setup failed: {e}"));
        return Ok(1);
    }

    if let Err(e) = update_status(store, &wt_name, generation_id, "active") {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }
    *st = store.read()?;

    if opts.emit_cd {
        println!("cd {}", sh_quote(&wt_path.to_string_lossy()));
    }

    Ok(0)
}

fn resolve_feature_supabase(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    state: &mut State,
    wt_path: &Path,
    project: Option<&ProjectConfig>,
    opts: &NewOpts<'_>,
) -> Result<SupabaseAllocation> {
    let start_supabase = project
        .and_then(|config| config.commands().setup())
        .is_none();
    let mode = match opts.sb_mode {
        FeatureSupabaseMode::Auto => {
            if !ensure_main_supabase(log, repo, store, state, false, start_supabase)? {
                ResolvedSupabaseMode::None
            } else if std::io::stdin().is_terminal()
                && confirm("Create a per-feature Supabase database? No reuses main. (y/N): ")?
            {
                ResolvedSupabaseMode::Isolated
            } else {
                ResolvedSupabaseMode::Shared
            }
        }
        FeatureSupabaseMode::Shared => {
            if !ensure_main_supabase(log, repo, store, state, true, start_supabase)? {
                anyhow::bail!("main worktree has no Supabase config to share");
            }
            ResolvedSupabaseMode::Shared
        }
        FeatureSupabaseMode::Isolated => ResolvedSupabaseMode::Isolated,
        FeatureSupabaseMode::None => ResolvedSupabaseMode::None,
    };

    match mode {
        ResolvedSupabaseMode::Shared => {
            if let Some(explicit) = opts.supabase_config {
                let root_path = state
                    .root
                    .as_ref()
                    .and_then(|root| root.supabase_config_path.as_deref());
                let explicit = supabase::resolve_target(Some(explicit), None, project)?;
                let stored = supabase::resolve_target(None, root_path, project)?;
                if explicit != stored {
                    anyhow::bail!(
                        "--supabase-config cannot override the main config in shared mode"
                    );
                }
            }
            let owner = state
                .primary_allocation_key()
                .ok_or_else(|| anyhow::anyhow!("primary worktree allocation is missing"))?;
            Ok(SupabaseAllocation::Shared {
                owner: owner.to_string(),
            })
        }
        ResolvedSupabaseMode::Isolated => {
            let stored = state
                .root
                .as_ref()
                .and_then(|root| root.supabase_config_path.as_deref());
            let target = supabase::resolve_target(opts.supabase_config, stored, project)?;
            if !supabase::has_config(wt_path, &target) {
                anyhow::bail!(
                    "supabase config not found: {}",
                    target.absolute_config_path(wt_path).display()
                );
            }
            Ok(SupabaseAllocation::Owned {
                project_id: String::new(),
                config_path: target.config_path_string(),
            })
        }
        ResolvedSupabaseMode::None => Ok(SupabaseAllocation::None),
    }
}

fn ensure_main_supabase(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    state: &mut State,
    allow_disabled: bool,
    start_stack: bool,
) -> Result<bool> {
    let Some(primary_key) = state.primary_allocation_key().map(str::to_string) else {
        return Ok(false);
    };
    let _primary_lifecycle = store.lock_allocation(&primary_key)?;
    *state = store.read()?;
    let primary = state.allocations[&primary_key].clone();
    let primary_generation = primary.generation_id;
    match primary.supabase {
        SupabaseAllocation::Owned { .. } => return Ok(true),
        SupabaseAllocation::None if !allow_disabled => return Ok(false),
        SupabaseAllocation::Shared { .. } => {
            anyhow::bail!("main worktree cannot share another Supabase stack")
        }
        SupabaseAllocation::None => {}
    }

    let stored = state
        .root
        .as_ref()
        .and_then(|root| root.supabase_config_path.as_deref());
    let main_path = Path::new(&primary.path);
    let project = ProjectConfig::load_for(&repo.config_root, main_path)?;
    let target = supabase::resolve_target(None, stored, project.as_ref())?;
    if !supabase::has_config(main_path, &target) {
        return Ok(false);
    }
    let project_id = supabase::project_id(main_path, &target)?;
    let config_path = target.config_path_string();
    let claims = supabase::port_claims(main_path, &target, 0)?;
    let established = store.update(|latest| {
        if let Some(stored) = latest
            .root
            .as_ref()
            .and_then(|root| root.supabase_config_path.as_deref())
            && stored != config_path
        {
            anyhow::bail!(
                "main Supabase target changed while ownership was being established: {stored:?} != {config_path:?}"
            );
        }
        let primary = latest.allocation_mut_if_generation(&primary_key, primary_generation)?;
        match &primary.supabase {
            SupabaseAllocation::Owned {
                project_id: existing_project_id,
                config_path: existing_config_path,
            } => {
                if existing_project_id != &project_id
                    || existing_config_path != &config_path
                    || !supabase_claims_match(&primary.ports, &claims)
                {
                    anyhow::bail!(
                        "main Supabase ownership changed while it was being established"
                    );
                }
                return Ok(false);
            }
            SupabaseAllocation::Shared { .. } => {
                anyhow::bail!("main worktree cannot share another Supabase stack");
            }
            SupabaseAllocation::None => {}
        }
        merge_port_assignments(&mut primary.ports, claims)?;
        primary.supabase = SupabaseAllocation::Owned {
            project_id,
            config_path: config_path.clone(),
        };
        if let Some(root) = latest.root.as_mut() {
            root.supabase_config_path = Some(config_path);
        }
        Ok(true)
    })?;
    *state = store.read()?;
    if established {
        log.infof("supabase: establishing shared main stack");
    }
    let primary = state.allocations[&primary_key].clone();
    let environment =
        envx::ResolvedEnvironment::build_before_setup(repo, state, &primary, project.as_ref())?;
    envx::sync_env_files(state, main_path, &primary, &environment)?;
    if !start_stack {
        return Ok(true);
    }
    supabase::ensure_started(main_path, &target)?;
    let environment = envx::ResolvedEnvironment::build(repo, state, &primary, project.as_ref())?;
    envx::sync_env_files(state, main_path, &primary, &environment)?;
    Ok(true)
}

fn isolated_supabase_port_claims(
    worktree_path: &Path,
    allocation: &SupabaseAllocation,
) -> Result<PortAssignments> {
    match allocation {
        SupabaseAllocation::Owned { config_path, .. } => {
            let target = supabase::Target::from_config_path(config_path)?;
            supabase::port_claims(worktree_path, &target, 0)
        }
        SupabaseAllocation::None | SupabaseAllocation::Shared { .. } => Ok(PortAssignments::new()),
    }
}

fn remove_failed_worktree(log: &ui::Logger, repo: &gitx::Repo, worktree_path: &Path) {
    if let Err(error) = worktree::remove(&repo.common_dir, worktree_path, true) {
        log.errorf(&format!(
            "cleanup failed: remove worktree {}: {error:#}",
            worktree_path.display()
        ));
    }
}

fn clear_failed_reservation(
    store: &StateStore,
    allocation_name: &str,
    generation_id: AllocationGeneration,
) -> Result<()> {
    let current = store.read()?;
    let Some(allocation) = current.allocations.get(allocation_name) else {
        return Ok(());
    };
    if allocation.generation_id != generation_id {
        anyhow::bail!("allocation {allocation_name:?} belongs to another creation operation");
    }

    let removal = store.update(|state| match state.allocations.get(allocation_name) {
        None => Ok(()),
        Some(allocation) if allocation.generation_id == generation_id => {
            state.remove_if_generation(allocation_name, generation_id)?;
            Ok(())
        }
        Some(_) => {
            anyhow::bail!("allocation {allocation_name:?} was replaced during reservation cleanup")
        }
    });
    let Err(removal_error) = removal else {
        return Ok(());
    };

    let current = store.read().map_err(|read_error| {
        anyhow::anyhow!(
            "remove persisted reservation: {removal_error:#}; reload state: {read_error:#}"
        )
    })?;
    match current.allocations.get(allocation_name) {
        None => Ok(()),
        Some(allocation) if allocation.generation_id == generation_id => Err(anyhow::anyhow!(
            "allocation {allocation_name:?} remains reserved: {removal_error:#}"
        )),
        Some(_) => {
            anyhow::bail!("allocation {allocation_name:?} was replaced during reservation cleanup")
        }
    }
}

fn supabase_claims_match(existing: &PortAssignments, expected: &PortAssignments) -> bool {
    let mut existing_claims = existing
        .iter()
        .filter(|(key, _)| key.as_str().starts_with("supabase."));
    existing_claims.clone().count() == expected.len()
        && existing_claims.all(|(key, port)| expected.get(key) == Some(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(root: &Path) -> gitx::Repo {
        gitx::Repo::new(
            root.to_path_buf(),
            root.join(".git"),
            root.join("main"),
            root.to_path_buf(),
            Some(root.join("main")),
        )
    }

    fn allocation(generation_id: AllocationGeneration) -> Allocation {
        Allocation {
            generation_id,
            name: "feature".to_string(),
            branch: "feature".to_string(),
            path: "/tmp/feature".to_string(),
            block: 1,
            offset: 100,
            ports: PortAssignments::new(),
            status: "creating".to_string(),
            created_at: "now".to_string(),
            supabase: SupabaseAllocation::None,
            setup: AllocationSetup::default(),
        }
    }

    #[test]
    fn failed_reservation_cleanup_removes_only_its_generation() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = StateStore::new(&repo(temp.path()));
        let expected = AllocationGeneration::new();
        store
            .update(|state| {
                state
                    .allocations
                    .insert("feature".to_string(), allocation(expected));
                Ok(())
            })
            .unwrap();

        clear_failed_reservation(&store, "feature", expected).unwrap();

        assert!(!store.read().unwrap().allocations.contains_key("feature"));
    }

    #[test]
    fn failed_reservation_cleanup_keeps_a_replacement_generation() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = StateStore::new(&repo(temp.path()));
        let stale = AllocationGeneration::new();
        let replacement = AllocationGeneration::new();
        store
            .update(|state| {
                state
                    .allocations
                    .insert("feature".to_string(), allocation(replacement));
                Ok(())
            })
            .unwrap();

        let error = clear_failed_reservation(&store, "feature", stale)
            .unwrap_err()
            .to_string();

        assert!(error.contains("another creation operation"), "{error}");
        assert_eq!(
            store.read().unwrap().allocations["feature"].generation_id,
            replacement
        );
    }
}
