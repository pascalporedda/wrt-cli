use anyhow::Result;
use chrono::SecondsFormat;
use std::io::IsTerminal;
use std::path::Path;

use crate::cli::FeatureSupabaseMode;
use crate::db;
use crate::envx;
use crate::gitx;
use crate::pm;
use crate::state::{Allocation, State, SupabaseAllocation};
use crate::supabase;
use crate::ui;
use crate::util::{confirm, run_argv_with_wrt_env, run_cmd, sh_quote, which};
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

pub struct SetupModes<'a> {
    pub install_mode: &'a str,
    pub db_mode: &'a str,
}

enum ResolvedSupabaseMode {
    Shared,
    Isolated,
    None,
}

pub fn cmd_new(
    log: &ui::Logger,
    repo: &gitx::Repo,
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

    let block = match st.allocate_block() {
        Ok(b) => b,
        Err(e) => {
            log.errorf(&format!("allocate port block: {e}"));
            return Ok(1);
        }
    };
    let offset = block * 100;

    log.infof(&format!(
        "creating worktree: {wt_name} ({br}) at {}",
        wt_path.display()
    ));

    worktree::ensure_dir(wt_path.parent().unwrap())?;

    if let Err(e) = worktree::add(&repo.common_dir, &wt_path, &br, opts.from_ref) {
        log.errorf(&format!("git worktree add failed: {e}"));
        return Ok(1);
    }

    let created_at = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let supabase = match resolve_feature_supabase(log, repo, st, &wt_path, &opts) {
        Ok(supabase) => supabase,
        Err(error) => {
            log.errorf(&format!("Supabase setup selection failed: {error}"));
            let _ = worktree::remove(&repo.common_dir, &wt_path, true);
            return Ok(2);
        }
    };
    let alloc = Allocation {
        name: wt_name.clone(),
        branch: br.clone(),
        path: wt_path.to_string_lossy().to_string(),
        block,
        offset,
        status: "creating".to_string(),
        created_at,
        supabase,
    };

    st.allocations.insert(wt_name.clone(), alloc.clone());
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    let setup = SetupModes {
        install_mode: opts.install_mode,
        db_mode: opts.db_mode,
    };
    if let Err(e) = setup_existing_worktree(log, repo, st, &wt_name, &wt_path, setup) {
        if let Some(a) = st.allocations.get_mut(&wt_name) {
            a.status = "failed".to_string();
        }
        let _ = st.save(&repo.common_dir);
        log.errorf(&format!("setup failed: {e}"));
        return Ok(1);
    }

    if let Some(a) = st.allocations.get_mut(&wt_name) {
        a.status = "active".to_string();
    }
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    if opts.emit_cd {
        println!("cd {}", sh_quote(&wt_path.to_string_lossy()));
    }

    Ok(0)
}

pub fn setup_existing_worktree(
    log: &ui::Logger,
    repo: &gitx::Repo,
    state: &mut State,
    allocation_name: &str,
    wt_path: &Path,
    modes: SetupModes<'_>,
) -> Result<()> {
    match worktree::copy_repo_env(&repo.root, wt_path) {
        Ok(true) => log.infof("copied .env from command worktree"),
        Ok(false) => {}
        Err(e) => log.infof(&format!("copy .env failed: {e}")),
    }
    match worktree::copy_repo_config(&repo.config_root, wt_path) {
        Ok(true) => log.infof("copied .wrt.json from managed root"),
        Ok(false) => {}
        Err(e) => log.infof(&format!("copy .wrt.json failed: {e}")),
    }

    let install = modes.install_mode.trim().to_lowercase();
    let db_mode = modes.db_mode.trim().to_lowercase();

    let mut allocation = state
        .allocations
        .get(allocation_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing allocation: {allocation_name}"))?;
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
            if let Err(e) =
                supabase::patch_config(wt_path, &target, &allocation.name, allocation.offset)
            {
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
        allocation.supabase = SupabaseAllocation::Owned {
            project_id,
            config_path: target.config_path_string(),
        };
        state
            .allocations
            .insert(allocation_name.to_string(), allocation.clone());
        state.save(&repo.common_dir)?;
    }

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

    if let Some((owner, target)) = supabase::allocation_target(state, &allocation)? {
        if which("supabase").is_none() {
            anyhow::bail!("supabase CLI not found in PATH");
        }
        log.infof(&format!(
            "supabase: ensuring {} stack is running",
            owner.name
        ));
        let owner = owner.clone();
        let owner_is_primary = state.is_primary_allocation(&owner);
        let owner_path = owner.path.clone();
        supabase::ensure_started(Path::new(&owner_path), &target)?;
        if owner_is_primary && owner.path != allocation.path {
            envx::sync_env_files(repo, state, Path::new(&owner_path), &owner)?;
        }
    }

    envx::sync_env_files(repo, state, wt_path, &allocation)
        .map_err(|e| anyhow::anyhow!("write env files: {e}"))?;

    if db_mode != "false" {
        if let Err(e) = maybe_run_db_setup(log, repo, state, &allocation, wt_path, &db_mode) {
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
    db_mode: &str,
) -> Result<()> {
    if db_mode == "auto" && matches!(alloc.supabase, SupabaseAllocation::Shared { .. }) {
        log.infof("supabase: skipping automatic DB reset for shared main database");
        return Ok(());
    }

    let resolved_target = supabase::allocation_target(state, alloc)?;
    let target = resolved_target.as_ref().map(|(_, target)| target.clone());
    let (kind_hint, reset_cmd) = db::command(&repo.config_root, wt_path, target.as_ref(), "reset");

    let Some(argv) = reset_cmd else {
        log.infof("no reset command known; run `wrt init` to generate .wrt.json");
        return Ok(());
    };

    if argv.is_empty() {
        return Ok(());
    }

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
            let code = run_argv_with_wrt_env(repo, state, &command_dir, alloc, &argv)?;
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
            let code = run_argv_with_wrt_env(repo, state, &command_dir, alloc, &argv)?;
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

fn resolve_feature_supabase(
    log: &ui::Logger,
    repo: &gitx::Repo,
    state: &mut State,
    wt_path: &Path,
    opts: &NewOpts<'_>,
) -> Result<SupabaseAllocation> {
    let mode = match opts.sb_mode {
        FeatureSupabaseMode::Auto => {
            if !ensure_main_supabase(log, repo, state, false)? {
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
            if !ensure_main_supabase(log, repo, state, true)? {
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
                let explicit = supabase::resolve_target(wt_path, Some(explicit), None)?;
                let stored = supabase::resolve_target(wt_path, None, root_path)?;
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
            let target = supabase::resolve_target(wt_path, opts.supabase_config, stored)?;
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
    state: &mut State,
    allow_disabled: bool,
) -> Result<bool> {
    let Some(primary_key) = state.primary_allocation_key().map(str::to_string) else {
        return Ok(false);
    };
    let primary = state.allocations[&primary_key].clone();
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
    let discovery_root = if repo.config_root.join(".wrt.json").is_file() {
        repo.config_root.as_path()
    } else {
        main_path
    };
    let target = supabase::resolve_target(discovery_root, None, stored)?;
    if !supabase::has_config(main_path, &target) {
        return Ok(false);
    }
    let project_id = supabase::project_id(main_path, &target)?;
    if let Some(root) = state.root.as_mut() {
        root.supabase_config_path = Some(target.config_path_string());
    }
    if let Some(primary) = state.allocations.get_mut(&primary_key) {
        primary.supabase = SupabaseAllocation::Owned {
            project_id,
            config_path: target.config_path_string(),
        };
    }
    state.save(&repo.common_dir)?;
    log.infof("supabase: establishing shared main stack");
    supabase::ensure_started(main_path, &target)?;
    let primary = state.allocations[&primary_key].clone();
    envx::sync_env_files(repo, state, main_path, &primary)?;
    Ok(true)
}
