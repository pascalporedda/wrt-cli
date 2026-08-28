use anyhow::Result;
use std::path::Path;

use crate::compose;
use crate::envx::ResolvedEnvironment;
use crate::gitx::Repo;
use crate::project::ProjectConfig;
use crate::state::State;
use crate::ui;
use crate::util::resolve_worktree_name;
use crate::worktree;

pub fn cmd_doctor(log: &ui::Logger, repo: &Repo, state: &State, name: Option<&str>) -> Result<i32> {
    let name = resolve_worktree_name(state, name, None);
    let Some(name) = name else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };
    let key = worktree::slug(&name);
    let Some(allocation) = state.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: {key:?}"));
        return Ok(2);
    };
    let project = ProjectConfig::load_for(&repo.config_root, Path::new(&allocation.path))?;
    let Some(project) = project else {
        log.infof(&format!(
            "{key}: no project config; Compose preflight is not configured"
        ));
        return Ok(0);
    };
    let environment =
        ResolvedEnvironment::build_before_setup(repo, state, allocation, Some(&project))?;
    let Some(report) = compose::inspect(repo, state, allocation, &project, &environment)? else {
        log.infof(&format!(
            "{key}: no Compose files declared; nothing to check"
        ));
        return Ok(0);
    };
    if report.is_safe() {
        log.infof(&format!("{key}: Compose isolation check passed"));
        return Ok(0);
    }
    log.errorf(&format!("{key}: Compose isolation check failed"));
    log.errorf(&compose::format_findings(&report));
    log.errorf(
        "fix Compose config errors, parameterize published host ports, and remove fixed container_name values",
    );
    Ok(1)
}
