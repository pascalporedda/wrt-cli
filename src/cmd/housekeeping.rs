use anyhow::{Context, Result, anyhow};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::gitx;
use crate::ui;

const TARGETS: [&str; 3] = ["main", "staging", "develop"];

#[derive(Debug)]
struct Candidate {
    kind: &'static str,
    name: String,
    delete_args: Vec<String>,
    reason: String,
}

pub fn cmd_housekeeping(log: &ui::Logger, repo: &gitx::Repo, apply: bool) -> Result<i32> {
    let _lock = apply
        .then(|| crate::worktree::lock_git_operations(&repo.common_dir))
        .transpose()?;
    let attached = attached_branches(&repo.root)?;
    let current = git_out(&repo.root, &["branch", "--show-current"]).unwrap_or_default();
    let mut candidates = Vec::new();

    for branch in git_lines(
        &repo.root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )? {
        if protected(&branch) || branch == current || attached.contains(&branch) {
            continue;
        }
        if let Some(target) = merged_target(&repo.root, &branch)? {
            candidates.push(Candidate {
                kind: "local",
                name: branch.clone(),
                delete_args: vec!["branch".into(), "-d".into(), branch],
                reason: format!("merged into {target}, not attached to a worktree"),
            });
        }
    }

    for branch in git_lines(
        &repo.root,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
    )? {
        let Some((remote, short)) = branch.split_once('/') else {
            continue;
        };
        if short == "HEAD" || protected(short) || attached.contains(short) {
            continue;
        }
        if let Some(target) = merged_target(&repo.root, &branch)? {
            candidates.push(Candidate {
                kind: "remote",
                name: branch.clone(),
                delete_args: vec![
                    "push".into(),
                    remote.into(),
                    "--delete".into(),
                    short.into(),
                ],
                reason: format!("merged into {target}, not attached to a worktree"),
            });
        }
    }

    if candidates.is_empty() {
        println!("nothing to clean");
        return Ok(0);
    }

    for c in candidates {
        println!("{} {}: {}", c.kind, c.name, c.reason);
        if apply {
            let args: Vec<&str> = c.delete_args.iter().map(String::as_str).collect();
            if let Err(e) = run_git(&repo.root, &args) {
                if c.kind == "remote" {
                    log.infof(&format!("warning: skipped {} {}: {e}", c.kind, c.name));
                    continue;
                }
                log.errorf(&format!("delete failed for {} {}: {e}", c.kind, c.name));
                return Ok(1);
            }
        }
    }

    Ok(0)
}

fn protected(branch: &str) -> bool {
    TARGETS.contains(&branch)
}

fn attached_branches(dir: &Path) -> Result<BTreeSet<String>> {
    let out = git_out(dir, &["worktree", "list", "--porcelain"])?;
    Ok(out
        .lines()
        .filter_map(|line| line.strip_prefix("branch refs/heads/"))
        .map(str::to_string)
        .collect())
}

fn merged_target(dir: &Path, branch: &str) -> Result<Option<&'static str>> {
    for target in TARGETS {
        let Some(target_ref) = target_ref(dir, target)? else {
            continue;
        };
        if git_ok(dir, &["merge-base", "--is-ancestor", branch, &target_ref])? {
            return Ok(Some(target));
        }
    }
    Ok(None)
}

fn target_ref(dir: &Path, target: &str) -> Result<Option<String>> {
    let local = format!("refs/heads/{target}");
    if git_ok(dir, &["show-ref", "--verify", "--quiet", &local])? {
        return Ok(Some(target.to_string()));
    }

    for remote in git_lines(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
    )? {
        if remote.ends_with(&format!("/{target}")) {
            return Ok(Some(remote));
        }
    }
    Ok(None)
}

fn git_lines(dir: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(git_out(dir, args)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_out(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("run git")?;
    if !out.status.success() {
        return Err(anyhow!("git command failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<bool> {
    Ok(Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .context("run git")?
        .success())
}

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    if git_ok(dir, args)? {
        Ok(())
    } else {
        Err(anyhow!("git command failed"))
    }
}
