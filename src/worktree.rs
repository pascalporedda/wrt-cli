use anyhow::{Context, Result, anyhow};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output};

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;

    for ch in s.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return "wrt".to_string();
    }
    out
}

pub fn normalize_branch(s: &str) -> String {
    let mut s = s.trim().to_string();
    if let Some(rest) = s.strip_prefix("refs/heads/") {
        s = rest.to_string();
    }
    // Avoid spaces; git is okay with more but keeping it strict helps automation.
    s.split_whitespace().collect::<Vec<_>>().join("-")
}

pub fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    Ok(())
}

pub fn add(git_dir: &Path, wt_path: &Path, branch: &str, from_ref: &str) -> Result<()> {
    let remotes = list_remotes(git_dir)?;
    let remote = pick_remote(&remotes);

    let remote_branch = if let Some(remote) = remote {
        let remote_head = format!("refs/heads/{branch}");
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        let advertised = git_out(
            git_dir,
            ["ls-remote", "--heads", remote, remote_head.as_str()],
        )?;

        if advertised.trim().is_empty() {
            run_git(git_dir, ["fetch", "--prune", remote])
                .with_context(|| format!("git fetch --prune {remote}"))?;
            None
        } else {
            let fetch_key = format!("remote.{remote}.fetch");
            if !git_ok(git_dir, ["config", "--get-all", fetch_key.as_str()])? {
                let fetch_refspec = format!("+refs/heads/*:refs/remotes/{remote}/*");
                run_git(
                    git_dir,
                    ["config", fetch_key.as_str(), fetch_refspec.as_str()],
                )?;
            }
            let refspec = format!("+{remote_head}:{remote_ref}");
            run_git(git_dir, ["fetch", remote, refspec.as_str()])
                .with_context(|| format!("git fetch {remote} {remote_head}"))?;
            Some(format!("{remote}/{branch}"))
        }
    } else {
        None
    };

    // Prefer an existing local branch, but attach it to the matching remote branch.
    if git_ok(
        git_dir,
        [
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )? {
        if let Some(upstream) = remote_branch.as_deref() {
            let local_oid = git_out(git_dir, ["rev-parse", branch])?;
            let upstream_oid = git_out(git_dir, ["rev-parse", upstream])?;
            let upstream_ref = format!("{branch}@{{upstream}}");
            let has_upstream = git_ok(
                git_dir,
                ["rev-parse", "--verify", "--quiet", upstream_ref.as_str()],
            )?;
            let can_fast_forward = local_oid != upstream_oid
                && git_ok(git_dir, ["merge-base", "--is-ancestor", branch, upstream])?;
            if !has_upstream || can_fast_forward {
                run_git(
                    git_dir,
                    ["branch", "--force", "--no-track", branch, upstream],
                )?;
            }
            run_git(git_dir, ["branch", "--set-upstream-to", upstream, branch])?;
        }
        return run_git(
            git_dir,
            [
                "worktree",
                "add",
                wt_path.to_string_lossy().as_ref(),
                branch,
            ],
        );
    }

    if let Some(start_point) = remote_branch {
        run_git(git_dir, ["branch", "--track", branch, start_point.as_str()])?;
        return run_git(
            git_dir,
            [
                "worktree",
                "add",
                wt_path.to_string_lossy().as_ref(),
                branch,
            ],
        );
    }

    run_git(
        git_dir,
        [
            "worktree",
            "add",
            "-b",
            branch,
            wt_path.to_string_lossy().as_ref(),
            from_ref,
        ],
    )
}

pub fn remove(git_dir: &Path, wt_path: &Path, force: bool) -> Result<()> {
    let mut args: Vec<String> = vec!["worktree".into(), "remove".into()];
    if force {
        args.push("--force".into());
    }
    args.push(wt_path.to_string_lossy().to_string());
    run_git(git_dir, args)
}

pub fn delete_branch(git_dir: &Path, branch: &str) -> Result<()> {
    run_git(git_dir, ["branch", "-D", branch])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamBranch {
    pub remote: String,
    pub branch: String,
}

pub fn branch_upstream(git_dir: &Path, branch: &str) -> Result<Option<UpstreamBranch>> {
    let local_ref = format!("refs/heads/{branch}");
    let out = git_out(
        git_dir,
        [
            "for-each-ref",
            "--format=%(upstream:remotename)%00%(upstream:remoteref)",
            local_ref.as_str(),
        ],
    )?;
    let Some((remote, remote_ref)) = out.trim().split_once('\0') else {
        return Ok(None);
    };
    let Some(remote_branch) = remote_ref.strip_prefix("refs/heads/") else {
        return Ok(None);
    };
    if remote.is_empty() || remote == "." || remote_branch.is_empty() {
        return Ok(None);
    }

    Ok(Some(UpstreamBranch {
        remote: remote.to_string(),
        branch: remote_branch.to_string(),
    }))
}

pub fn delete_remote_branch(git_dir: &Path, remote: &str, remote_branch: &str) -> Result<()> {
    run_git(git_dir, ["push", remote, "--delete", remote_branch])
}

pub fn is_dirty(wt_path: &Path) -> Result<bool> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(wt_path)
        .output()
        .context("git status")?;
    if !out.status.success() {
        return Err(anyhow!("git status failed"));
    }
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

pub fn copy_repo_env(repo_root: &Path, wt_path: &Path) -> Result<bool> {
    copy_repo_file(repo_root, wt_path, ".env")
}

pub fn copy_repo_env_at(repo_root: &Path, wt_path: &Path, relative_dir: &Path) -> Result<bool> {
    let src = repo_root.join(relative_dir).join(".env");
    let dst = wt_path.join(relative_dir).join(".env");
    copy_file(&src, &dst)
}

pub fn copy_repo_config(repo_root: &Path, wt_path: &Path) -> Result<bool> {
    copy_repo_file(repo_root, wt_path, ".wrt.json")
}

fn copy_repo_file(repo_root: &Path, wt_path: &Path, file_name: &str) -> Result<bool> {
    let src = repo_root.join(file_name);
    let dst = wt_path.join(file_name);
    copy_file(&src, &dst)
}

fn copy_file(src: &Path, dst: &Path) -> Result<bool> {
    if !src.is_file() {
        return Ok(false);
    }
    if dst.exists() {
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::copy(src, dst).with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    Ok(true)
}

fn run_git<I, S>(dir: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let out = run_git_output(dir, &args)?;
    if !out.status.success() {
        return Err(git_failure(dir, &args, &out));
    }
    relay_output(&out);
    Ok(())
}

fn git_ok<I, S>(dir: &Path, args: I) -> Result<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let out = run_git_output(dir, &args)?;
    Ok(out.status.success())
}

fn git_out<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let out = run_git_output(dir, &args)?;
    if !out.status.success() {
        return Err(git_failure(dir, &args, &out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn run_git_output(git_dir: &Path, args: &[OsString]) -> Result<Output> {
    Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(args)
        .output()
        .map_err(|e| anyhow!("run {}: {e}", git_command(git_dir, args)))
}

fn git_failure(git_dir: &Path, args: &[OsString], out: &Output) -> anyhow::Error {
    let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let detail = if detail.is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        detail
    };
    if detail.is_empty() {
        anyhow!("{} failed with {}", git_command(git_dir, args), out.status)
    } else {
        anyhow!(
            "{} failed with {}: {detail}",
            git_command(git_dir, args),
            out.status
        )
    }
}

fn git_command(git_dir: &Path, args: &[OsString]) -> String {
    let mut parts = vec![
        "git".to_string(),
        "--git-dir".to_string(),
        shellish(git_dir.as_os_str()),
    ];
    parts.extend(args.iter().map(|arg| shellish(arg.as_os_str())));
    parts.join(" ")
}

fn shellish(s: &OsStr) -> String {
    let s = s.to_string_lossy();
    if s.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=@".contains(ch))
    {
        s.to_string()
    } else {
        format!("{s:?}")
    }
}

fn relay_output(out: &Output) {
    let _ = io::stdout().write_all(&out.stdout);
    let _ = io::stderr().write_all(&out.stderr);
}

fn list_remotes(git_dir: &Path) -> Result<Vec<String>> {
    let out = git_out(git_dir, ["remote"])?;
    Ok(out
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

fn pick_remote(remotes: &[String]) -> Option<&str> {
    if remotes.is_empty() {
        return None;
    }
    for r in remotes {
        if r == "origin" {
            return Some(r.as_str());
        }
    }
    remotes.first().map(|r| r.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slug("a/gpt/fix-login-timeout"), "a-gpt-fix-login-timeout");
        assert_eq!(slug("  Hello   World  "), "hello-world");
        assert_eq!(slug("***"), "wrt");
    }

    #[test]
    fn normalize_branch_basic() {
        assert_eq!(normalize_branch("refs/heads/a/b"), "a/b");
        assert_eq!(normalize_branch("hello world"), "hello-world");
    }
}
