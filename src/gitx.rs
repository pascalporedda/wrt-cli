use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
    pub common_dir: PathBuf,
}

pub fn detect_repo(cwd: &Path) -> Result<Repo> {
    let root =
        git_out(cwd, ["rev-parse", "--show-toplevel"]).context("git rev-parse --show-toplevel")?;
    let common = git_out(cwd, ["rev-parse", "--git-common-dir"])
        .context("git rev-parse --git-common-dir")?;

    let workdir_root = PathBuf::from(root.trim());
    let mut common_dir = PathBuf::from(common.trim());
    if common_dir.as_os_str().is_empty() {
        return Err(anyhow!("empty --git-common-dir"));
    }
    if !common_dir.is_absolute() {
        common_dir = workdir_root.join(common_dir);
    }

    // When invoked from inside a git worktree, `--show-toplevel` points at the worktree root,
    // but wrt's runtime artifacts and config live at the main workdir root (parent of the common
    // git dir, typically `<repo>/.git`).
    let root = match common_dir.file_name().and_then(|s| s.to_str()) {
        Some(".git") => common_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(workdir_root),
        _ => workdir_root,
    };

    Ok(Repo { root, common_dir })
}

pub fn ensure_info_exclude(common_dir: &Path, patterns: &[&str]) -> Result<()> {
    let exclude_path = common_dir.join("info").join("exclude");
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }

    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();

    let has = |p: &str| -> bool { existing.lines().any(|line| line.trim() == p.trim()) };

    let mut out = String::new();
    out.push_str(&existing);
    let mut changed = false;
    if !existing.is_empty() && !existing.ends_with('\n') {
        out.push('\n');
        changed = true;
    }

    for p in patterns {
        let p = p.trim();
        if p.is_empty() || has(p) {
            continue;
        }
        out.push_str(p);
        out.push('\n');
        changed = true;
    }

    if !changed {
        return Ok(());
    }

    fs::write(&exclude_path, out.as_bytes())
        .with_context(|| format!("write {}", exclude_path.display()))?;
    Ok(())
}

fn git_out<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
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
