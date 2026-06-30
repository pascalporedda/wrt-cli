use anyhow::Result;
use chrono::SecondsFormat;
use std::io::IsTerminal;
use std::path::Path;

use crate::db;
use crate::envx;
use crate::gitx;
use crate::pm;
use crate::state::{Allocation, State};
use crate::supabase;
use crate::ui;
use crate::util::{confirm, run_argv_with_wrt_env, run_cmd, sh_quote, which};
use crate::worktree;

pub struct NewOpts<'a> {
    pub name: &'a str,
    pub from_ref: &'a str,
    pub branch: Option<&'a str>,
    pub install_mode: &'a str,
    pub sb_mode: &'a str,
    pub db_mode: &'a str,
    pub emit_cd: bool,
}

pub struct SetupModes<'a> {
    pub install_mode: &'a str,
    pub sb_mode: &'a str,
    pub db_mode: &'a str,
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
    let alloc = Allocation {
        name: wt_name.clone(),
        branch: br.clone(),
        path: wt_path.to_string_lossy().to_string(),
        block,
        offset,
        status: "creating".to_string(),
        created_at,
    };

    st.allocations.insert(wt_name.clone(), alloc.clone());
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    let setup = SetupModes {
        install_mode: opts.install_mode,
        sb_mode: opts.sb_mode,
        db_mode: opts.db_mode,
    };
    if let Err(e) = setup_existing_worktree(log, repo, &alloc, &wt_path, setup) {
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
    alloc: &Allocation,
    wt_path: &Path,
    modes: SetupModes<'_>,
) -> Result<()> {
    match worktree::copy_repo_env(&repo.config_root, wt_path) {
        Ok(true) => log.infof("copied .env from config worktree"),
        Ok(false) => {}
        Err(e) => log.infof(&format!("copy .env failed: {e}")),
    }
    match worktree::copy_repo_config(&repo.config_root, wt_path) {
        Ok(true) => log.infof("copied .wrt.json from config worktree"),
        Ok(false) => {}
        Err(e) => log.infof(&format!("copy .wrt.json failed: {e}")),
    }

    if let Err(e) = envx::write_env_file(repo, wt_path, alloc) {
        anyhow::bail!("write env file: {e}");
    }

    let sb = modes.sb_mode.trim().to_lowercase();
    let install = modes.install_mode.trim().to_lowercase();
    let db_mode = modes.db_mode.trim().to_lowercase();

    if sb == "true" || (sb == "auto" && supabase::has_config(wt_path)) {
        log.infof("supabase detected: patching config for isolation (project_id + ports)");
        if let Err(e) = supabase::patch_config(wt_path, &alloc.name, alloc.offset) {
            anyhow::bail!("supabase patch failed: {e}");
        }
        let _ = run_cmd(
            wt_path,
            "git",
            &["update-index", "--skip-worktree", "supabase/config.toml"],
        );
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

    if sb == "true" || (sb == "auto" && supabase::has_config(wt_path)) {
        if which("supabase").is_none() {
            log.infof("supabase CLI not found; skipping `supabase start` (config patched)");
        } else {
            log.infof("supabase start (isolated ports, project_id suffix)");
            if let Err(e) = run_cmd(wt_path, "supabase", &["start"]) {
                anyhow::bail!("supabase start failed: {e}");
            }
        }
    }

    if db_mode != "false" {
        if let Err(e) = maybe_run_db_setup(log, repo, alloc, wt_path, &db_mode) {
            anyhow::bail!("db setup failed: {e}");
        }
    }

    Ok(())
}

fn maybe_run_db_setup(
    log: &ui::Logger,
    repo: &gitx::Repo,
    alloc: &Allocation,
    wt_path: &Path,
    db_mode: &str,
) -> Result<()> {
    let (kind_hint, reset_cmd) = db::command(&repo.config_root, wt_path, "reset");

    let Some(argv) = reset_cmd else {
        log.infof("no reset command known; run `wrt init` to generate .wrt.json");
        return Ok(());
    };

    if argv.is_empty() {
        return Ok(());
    }

    let label = kind_hint.as_deref().unwrap_or("database");
    let cmd_str = argv.join(" ");

    match db_mode {
        "true" => {
            log.infof(&format!("{label}: running db setup: {cmd_str}"));
            let code = run_argv_with_wrt_env(repo, wt_path, alloc, &argv)?;
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
            let code = run_argv_with_wrt_env(repo, wt_path, alloc, &argv)?;
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
