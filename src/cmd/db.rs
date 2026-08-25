use anyhow::Result;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::cli::DbAction;
use crate::db;
use crate::gitx;
use crate::project::ProjectConfig;
use crate::state::State;
use crate::state::SupabaseAllocation;
use crate::supabase;
use crate::ui;
use crate::util::{confirm, infer_worktree_from_cwd, run_argv_with_wrt_env};
use crate::worktree;

pub fn cmd_db(
    log: &ui::Logger,
    repo: &gitx::Repo,
    st: &State,
    name: Option<&str>,
    worktree_arg: Option<&str>,
    action: DbAction,
) -> Result<i32> {
    let mut resolved = worktree_arg
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));

    if resolved.is_none() {
        resolved = infer_worktree_from_cwd(st);
    }

    let Some(resolved) = resolved else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&resolved);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    let wt_path = PathBuf::from(&a.path);
    let (op, yes, print) = match action {
        DbAction::Reset { yes, print } => ("reset", yes, print),
        DbAction::Seed { print } => ("seed", false, print),
        DbAction::Migrate { print } => ("migrate", false, print),
    };
    let resolved_target = supabase::allocation_target(st, a)?;
    let target = resolved_target.as_ref().map(|(_, target)| target.clone());
    let project = ProjectConfig::load_for(&repo.config_root, &wt_path)?;
    let (kind_hint, cmd) = db::command(project.as_ref(), &wt_path, target.as_ref(), op);

    let Some(argv) = cmd else {
        let label = kind_hint.as_deref().unwrap_or("database");
        log.errorf(&format!(
            "{label}: no {op} command known; run `wrt init` to generate .wrt.json"
        ));
        return Ok(2);
    };
    let label = kind_hint.as_deref().unwrap_or("database");
    let cmd_str = argv.join(" ");

    if let SupabaseAllocation::Shared { owner } = &a.supabase {
        log.infof(&format!(
            "{label}: operation targets the shared database owned by {owner}"
        ));
    }

    if print {
        println!("{cmd_str}");
        return Ok(0);
    }

    if op == "reset" && !yes {
        if !std::io::stdin().is_terminal() {
            log.errorf(&format!(
                "{label}: refusing to run reset non-interactively; pass `--yes` to confirm"
            ));
            return Ok(2);
        } else if !confirm(&format!(
            "{label}: run DB reset now? This may delete local data. [{cmd_str}] (y/N): "
        ))? {
            log.infof(&format!("{label}: skipping reset"));
            return Ok(0);
        }
    }

    log.infof(&format!("{label}: running: {cmd_str}"));
    let command_dir = if kind_hint.as_deref() == Some("supabase") {
        supabase::command_workdir(resolved_target.as_ref(), &wt_path)
    } else {
        wt_path
    };
    run_argv_with_wrt_env(repo, st, &command_dir, a, project.as_ref(), &argv)
}
