use anyhow::Result;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::cli::DbAction;
use crate::db;
use crate::gitx;
use crate::project::ProjectConfig;
use crate::state::SupabaseAllocation;
use crate::state::{State, StateStore};
use crate::supabase;
use crate::ui;
use crate::util::{confirm, resolve_worktree_name, run_argv_with_wrt_env, sh_quote};
use crate::worktree;

pub fn cmd_db(
    log: &ui::Logger,
    repo: &gitx::Repo,
    store: &StateStore,
    st: &State,
    name: Option<&str>,
    worktree_arg: Option<&str>,
    action: DbAction,
) -> Result<i32> {
    let resolved = resolve_worktree_name(st, name, worktree_arg);

    let Some(resolved) = resolved else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&resolved);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };
    let expected_generation = a.generation_id;
    let expected_supabase = a.supabase.clone();
    let expected_owner = match &a.supabase {
        SupabaseAllocation::Shared { owner } => Some(owner_generation(st, owner)?),
        _ => None,
    };
    let _selected_lifecycle = match &a.supabase {
        SupabaseAllocation::Shared { .. } => store.lock_allocation_shared(&key)?,
        _ => store.lock_allocation(&key)?,
    };
    let _owner_lifecycle = expected_owner
        .as_ref()
        .map(|(owner, _)| store.lock_allocation(owner))
        .transpose()?;
    let latest = store.read()?;
    let Some(a) = latest.allocations.get(&key) else {
        log.errorf(&format!(
            "worktree was removed before database command: \"{key}\""
        ));
        return Ok(2);
    };
    if a.generation_id != expected_generation || a.supabase != expected_supabase {
        log.errorf(&format!(
            "worktree was replaced before database command: \"{key}\""
        ));
        return Ok(2);
    }
    if let Some((owner, generation)) = &expected_owner {
        let Some(latest_owner) = latest.allocations.get(owner) else {
            log.errorf(&format!("shared database owner was removed: {owner:?}"));
            return Ok(2);
        };
        if latest_owner.generation_id != *generation {
            log.errorf(&format!("shared database owner was replaced: {owner:?}"));
            return Ok(2);
        }
    }

    let wt_path = PathBuf::from(&a.path);
    let (op, yes, print) = match action {
        DbAction::Reset { yes, print } => ("reset", yes, print),
        DbAction::Seed { print } => ("seed", false, print),
        DbAction::Migrate { print } => ("migrate", false, print),
    };
    let resolved_target = supabase::allocation_target(&latest, a)?;
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
    let cmd_str = argv
        .iter()
        .map(|argument| sh_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");

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
    run_argv_with_wrt_env(repo, &latest, &command_dir, a, project.as_ref(), &argv)
}

fn owner_generation(
    state: &State,
    owner: &str,
) -> Result<(String, crate::state::AllocationGeneration)> {
    let allocation = state
        .allocations
        .get(owner)
        .ok_or_else(|| anyhow::anyhow!("shared database owner is missing: {owner:?}"))?;
    Ok((owner.to_string(), allocation.generation_id))
}
