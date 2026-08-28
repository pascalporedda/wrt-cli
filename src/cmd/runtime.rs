use anyhow::{Result, bail};
use std::path::Path;

use crate::cli::RuntimeAction;
use crate::compose;
use crate::envx::ResolvedEnvironment;
use crate::gitx::Repo;
use crate::project::ProjectConfig;
use crate::state::{State, StateStore};
use crate::ui;
use crate::util::{resolve_worktree_name, run_project_command};
use crate::worktree;

pub fn cmd_runtime(
    log: &ui::Logger,
    repo: &Repo,
    store: &StateStore,
    snapshot: &State,
    name: Option<&str>,
    worktree: Option<&str>,
    action: RuntimeAction,
) -> Result<i32> {
    let name = resolve_worktree_name(snapshot, name, worktree);
    let Some(name) = name else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&name);
    let Some(expected) = snapshot.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: {key:?}"));
        return Ok(2);
    };
    let generation_id = expected.generation_id;

    let _lifecycle = store.lock_allocation_shared(&key)?;
    let state = store.read()?;
    let Some(allocation) = state.allocations.get(&key) else {
        bail!("allocation {key:?} was removed before runtime command started");
    };
    if allocation.generation_id != generation_id {
        bail!("allocation {key:?} was replaced before runtime command started");
    }

    let worktree_root = Path::new(&allocation.path);
    if !worktree_root.is_dir() {
        log.errorf(&format!(
            "worktree path is missing: {}",
            worktree_root.display()
        ));
        return Ok(2);
    }
    let Some(project) = ProjectConfig::load_for(&repo.config_root, worktree_root)? else {
        log.errorf("no project config; run `wrt init` to generate .wrt.json");
        return Ok(2);
    };
    let (action_name, command) = match action {
        RuntimeAction::Start => ("start", project.commands().start()),
        RuntimeAction::Stop => ("stop", project.commands().stop()),
        RuntimeAction::Status => ("status", project.commands().status()),
    };
    let Some(command) = command else {
        log.errorf(&format!(
            "project runtime {} command is not declared in .wrt.json",
            action_name
        ));
        return Ok(2);
    };

    let environment =
        ResolvedEnvironment::build_before_setup(repo, &state, allocation, Some(&project))?;
    if action == RuntimeAction::Start
        && let Some(report) = compose::inspect(repo, &state, allocation, &project, &environment)?
        && !report.is_safe()
    {
        log.errorf("Compose isolation preflight failed");
        log.errorf(&compose::format_findings(&report));
        log.errorf(
            "fix Compose config errors, parameterize published host ports, and remove fixed container_name values",
        );
        return Ok(1);
    }

    run_project_command(&state, worktree_root, allocation, &environment, command)
}
