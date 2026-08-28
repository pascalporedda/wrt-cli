use anyhow::Result;
use std::io::IsTerminal;
use std::path::Path;

use crate::compose;
use crate::db;
use crate::envx;
use crate::gitx;
use crate::pm;
use crate::project::ProjectConfig;
use crate::state::{Allocation, AllocationGeneration, State, StateStore, SupabaseAllocation};
use crate::supabase;
use crate::ui;
use crate::util::{confirm, run_argv_with_wrt_env, run_cmd, run_project_command, which};
use crate::worktree;

#[derive(Clone, Copy)]
pub(crate) struct SetupModes<'a> {
    pub install_mode: &'a str,
    pub db_mode: &'a str,
}

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

pub(crate) fn setup_existing_worktree(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    state: &mut State,
    expected_allocation: &Allocation,
    project: Option<&ProjectConfig>,
    modes: SetupModes<'_>,
) -> Result<()> {
    let allocation_name = expected_allocation.name.as_str();
    let generation_id = expected_allocation.generation_id;
    let wt_path = Path::new(&expected_allocation.path);
    let _lifecycle = store.lock_allocation(allocation_name)?;
    *state = store.read()?;
    let mut allocation = state
        .allocations
        .get(allocation_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing allocation: {allocation_name}"))?;
    if allocation.generation_id != generation_id {
        anyhow::bail!("allocation {allocation_name:?} was replaced before setup started");
    }

    gitx::ensure_hooks_path(&repo.common_dir)?;

    match worktree::copy_repo_env(&repo.root, wt_path) {
        Ok(true) => log.infof("copied .env from command worktree"),
        Ok(false) => {}
        Err(e) => log.infof(&format!("copy .env failed: {e}")),
    }
    if let Some(command) = project.and_then(|config| config.commands().setup()) {
        let environment =
            envx::ResolvedEnvironment::build_before_setup(repo, state, &allocation, project)?;
        if let Some(project) = project
            && let Some(report) = compose::inspect(repo, state, &allocation, project, &environment)?
            && !report.is_safe()
        {
            anyhow::bail!(
                "Compose isolation preflight failed:\n{}\nfix Compose config errors, parameterize published host ports, and remove fixed container_name values",
                compose::format_findings(&report)
            );
        }
        envx::sync_env_files(state, wt_path, &allocation, &environment)
            .map_err(|error| anyhow::anyhow!("write env files: {error}"))?;
        let code = run_project_command(state, wt_path, &allocation, &environment, command)?;
        if code != 0 {
            anyhow::bail!("project setup command exited with status {code}");
        }
        return Ok(());
    }

    let install = modes.install_mode.trim().to_lowercase();
    let db_mode = modes.db_mode.trim().to_lowercase();
    let allocation_target = supabase::allocation_target(state, &allocation)?;
    if let Some((_, target)) = allocation_target.as_ref() {
        if !supabase::has_config(wt_path, target) {
            anyhow::bail!(
                "supabase config not found: {}",
                target.absolute_config_path(wt_path).display()
            );
        }
        match worktree::copy_repo_env_at(&repo.main_worktree, wt_path, target.relative_workdir()) {
            Ok(true) => log.infof("copied Supabase workdir .env from main"),
            Ok(false) => {}
            Err(e) => log.infof(&format!("copy Supabase workdir .env failed: {e}")),
        }
    }
    if let SupabaseAllocation::Owned { config_path, .. } = &allocation.supabase {
        let target = supabase::Target::from_config_path(config_path)?;
        if !state.is_primary_allocation(&allocation) {
            log.infof("supabase detected: patching config for isolation (project_id + ports)");
            if let Err(e) = supabase::patch_config_to_claims(
                wt_path,
                &target,
                &allocation.name,
                allocation.offset,
                &allocation.ports,
            ) {
                anyhow::bail!("supabase patch failed: {e}");
            }
            let config_path = target.config_path_string();
            let _ = run_cmd(
                wt_path,
                "git",
                &["update-index", "--skip-worktree", &config_path],
            );
        }
        let project_id = supabase::project_id(wt_path, &target)?;
        let config_path = target.config_path_string();
        store.update(|latest| {
            let allocation = latest.allocation_mut_if_generation(allocation_name, generation_id)?;
            allocation.supabase = SupabaseAllocation::Owned {
                project_id,
                config_path,
            };
            Ok(())
        })?;
        *state = store.read()?;
        allocation = state
            .allocations
            .get(allocation_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("allocation {allocation_name:?} no longer exists"))?;
        if allocation.generation_id != generation_id {
            anyhow::bail!("allocation {allocation_name:?} was replaced during setup");
        }
    }

    let environment =
        envx::ResolvedEnvironment::build_before_setup(repo, state, &allocation, project)?;
    envx::sync_env_files(state, wt_path, &allocation, &environment)
        .map_err(|e| anyhow::anyhow!("write env files: {e}"))?;

    if install == "true" || (install == "auto" && pm::has_project(wt_path)) {
        if let Some((cmd, args)) = pm::detect_install_command(wt_path) {
            log.infof(&format!("install: {cmd} {}", args.join(" ")));
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            if let Err(e) = run_cmd(wt_path, &cmd, &arg_refs) {
                anyhow::bail!("install failed: {e}");
            }
        } else {
            log.infof("no package manager detected; skipping install");
        }
    }

    if let Some((owner, _target)) = supabase::allocation_target(state, &allocation)? {
        if which("supabase").is_none() {
            anyhow::bail!("supabase CLI not found in PATH");
        }
        let owner_name = owner.name.clone();
        let owner_generation = owner.generation_id;
        let _owner_lifecycle = if owner_name != allocation.name {
            Some(store.lock_allocation(&owner_name)?)
        } else {
            None
        };
        if _owner_lifecycle.is_some() {
            *state = store.read()?;
        }
        let owner = state
            .allocations
            .get(&owner_name)
            .filter(|owner| owner.generation_id == owner_generation)
            .ok_or_else(|| anyhow::anyhow!("shared Supabase owner {owner_name:?} was replaced"))?;
        let target = match &owner.supabase {
            SupabaseAllocation::Owned { config_path, .. } => {
                supabase::Target::from_config_path(config_path)?
            }
            _ => anyhow::bail!("shared Supabase owner {owner_name:?} is not owned"),
        };
        log.infof(&format!(
            "supabase: ensuring {} stack is running",
            owner.name
        ));
        let owner = owner.clone();
        let owner_is_primary = state.is_primary_allocation(&owner);
        let owner_path = owner.path.clone();
        supabase::ensure_started(Path::new(&owner_path), &target)?;
        if owner_is_primary && owner.path != allocation.path {
            let owner_project = ProjectConfig::load_for(&repo.config_root, Path::new(&owner_path))?;
            let owner_environment =
                envx::ResolvedEnvironment::build(repo, state, &owner, owner_project.as_ref())?;
            envx::sync_env_files(state, Path::new(&owner_path), &owner, &owner_environment)?;
        }
    }

    let environment = envx::ResolvedEnvironment::build(repo, state, &allocation, project)?;
    envx::sync_env_files(state, wt_path, &allocation, &environment)
        .map_err(|e| anyhow::anyhow!("write env files: {e}"))?;

    if db_mode != "false" {
        if let Err(e) =
            maybe_run_db_setup(log, repo, state, &allocation, wt_path, project, &db_mode)
        {
            anyhow::bail!("db setup failed: {e}");
        }
    }

    Ok(())
}

fn maybe_run_db_setup(
    log: &ui::Logger,
    repo: &gitx::Repo,
    state: &State,
    alloc: &Allocation,
    wt_path: &Path,
    project: Option<&ProjectConfig>,
    db_mode: &str,
) -> Result<()> {
    if db_mode == "auto" && matches!(alloc.supabase, SupabaseAllocation::Shared { .. }) {
        log.infof("supabase: skipping automatic DB reset for shared main database");
        return Ok(());
    }

    let resolved_target = supabase::allocation_target(state, alloc)?;
    let target = resolved_target.as_ref().map(|(_, target)| target.clone());
    let (kind_hint, reset_cmd) = db::command(project, wt_path, target.as_ref(), "reset");

    let Some(argv) = reset_cmd else {
        log.infof("no reset command known; run `wrt init` to generate .wrt.json");
        return Ok(());
    };

    let label = kind_hint.as_deref().unwrap_or("database");
    let cmd_str = argv.join(" ");
    let command_dir = if kind_hint.as_deref() == Some("supabase") {
        supabase::command_workdir(resolved_target.as_ref(), wt_path)
    } else {
        wt_path.to_path_buf()
    };

    match db_mode {
        "true" => {
            if matches!(alloc.supabase, SupabaseAllocation::Shared { .. }) {
                log.infof("supabase: explicitly resetting the shared main database");
            }
            log.infof(&format!("{label}: running db setup: {cmd_str}"));
            let code = run_argv_with_wrt_env(repo, state, &command_dir, alloc, project, &argv)?;
            if code != 0 {
                anyhow::bail!("command failed");
            }
        }
        "auto" => {
            if !std::io::stdin().is_terminal() {
                log.infof(&format!(
                    "{label}: db setup available ({cmd_str}) but skipping in non-interactive mode; rerun with `--db true` to run"
                ));
                return Ok(());
            }

            if !confirm(&format!(
                "{label}: run DB reset/seed now? This may delete local data. [{cmd_str}] (y/N): "
            ))? {
                log.infof(&format!("{label}: skipping db setup"));
                return Ok(());
            }

            log.infof(&format!("{label}: running db setup: {cmd_str}"));
            let code = run_argv_with_wrt_env(repo, state, &command_dir, alloc, project, &argv)?;
            if code != 0 {
                anyhow::bail!("command failed");
            }
        }
        "false" => {}
        _ => {
            log.infof("invalid --db value (expected auto|true|false); skipping db setup");
        }
    }

    Ok(())
}

pub(crate) fn update_status(
    store: &StateStore,
    allocation_name: &str,
    generation_id: AllocationGeneration,
    status: &str,
) -> Result<()> {
    store.update(|state| {
        let allocation = state.allocation_mut_if_generation(allocation_name, generation_id)?;
        allocation.status = status.to_string();
        Ok(())
    })
}
