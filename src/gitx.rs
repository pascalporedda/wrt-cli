use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Repo {
    /// Directory used for normal git commands.
    /// Usually `<managed-root>/main`, but falls back to another linked checkout if main is absent.
    pub root: PathBuf,
    pub common_dir: PathBuf,
    pub managed_root: PathBuf,
    pub invocation_root: Option<PathBuf>,
    /// Directory for shared wrt config such as `.wrt.json`.
    pub config_root: PathBuf,
    pub main_worktree: PathBuf,
    pub worktree_parent: PathBuf,
}

impl Repo {
    pub fn new(
        managed_root: PathBuf,
        common_dir: PathBuf,
        main_worktree: PathBuf,
        worktree_parent: PathBuf,
        invocation_root: Option<PathBuf>,
    ) -> Repo {
        let root = command_root(
            &common_dir,
            &managed_root,
            &main_worktree,
            invocation_root.as_deref(),
        );
        let config_root = managed_root.clone();
        Repo {
            root: root.clone(),
            common_dir,
            managed_root,
            invocation_root,
            config_root,
            main_worktree,
            worktree_parent,
        }
    }

    pub fn worktree_path(&self, name: &str) -> PathBuf {
        self.worktree_parent.join(name)
    }
}

pub fn detect_repo(cwd: &Path) -> Result<Repo> {
    if let Some(repo) = detect_managed_root_container(cwd)? {
        return Ok(repo);
    }

    let root =
        git_out(cwd, ["rev-parse", "--show-toplevel"]).context("git rev-parse --show-toplevel")?;
    let common = git_out(cwd, ["rev-parse", "--git-common-dir"])
        .context("git rev-parse --git-common-dir")?;

    let workdir_root = PathBuf::from(root.trim());
    let invocation_root = workdir_root.clone();
    let mut common_dir = PathBuf::from(common.trim());
    if common_dir.as_os_str().is_empty() {
        return Err(anyhow!("empty --git-common-dir"));
    }
    if !common_dir.is_absolute() {
        common_dir = workdir_root.join(common_dir);
    }

    let meta = ManagedRootMeta::read(&common_dir).ok_or_else(|| {
        anyhow!("not a wrt managed root; run `wrt clone <repo>` or `wrt root init <source> --root <dir>`")
    })?;
    Ok(repo_from_meta(meta, common_dir, Some(invocation_root)))
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

pub fn ensure_hooks_path(common_dir: &Path) -> Result<()> {
    let configured = Command::new("git")
        .arg("--git-dir")
        .arg(common_dir)
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output()
        .context("read core.hooksPath")?;
    if configured.status.success() && !configured.stdout.is_empty() {
        return Ok(());
    }

    let hooks_dir = common_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).with_context(|| format!("mkdir {}", hooks_dir.display()))?;
    let status = Command::new("git")
        .arg("--git-dir")
        .arg(common_dir)
        .args(["config", "--local", "core.hooksPath"])
        .arg(&hooks_dir)
        .status()
        .context("configure core.hooksPath")?;
    if !status.success() {
        return Err(anyhow!("git config core.hooksPath failed"));
    }
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

fn detect_managed_root_container(cwd: &Path) -> Result<Option<Repo>> {
    let common_dir = cwd.join(".git");
    if !common_dir.is_dir() {
        return Ok(None);
    }
    if !git_dir_is_bare(&common_dir)? {
        return Ok(None);
    }
    let Some(meta) = ManagedRootMeta::read(&common_dir) else {
        return Ok(None);
    };
    let managed_root = meta.managed_root.unwrap_or_else(|| cwd.to_path_buf());
    let main_worktree = meta
        .main_worktree
        .unwrap_or_else(|| managed_root.join("main"));
    let worktree_parent = meta.worktrees_path.unwrap_or_else(|| managed_root.clone());
    Ok(Some(Repo::new(
        managed_root,
        common_dir,
        main_worktree,
        worktree_parent,
        None,
    )))
}

fn git_dir_is_bare(git_dir: &Path) -> Result<bool> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .context("run git")?;
    if !out.status.success() {
        return Ok(false);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "true")
}

#[derive(Debug, Default)]
struct ManagedRootMeta {
    managed_root: Option<PathBuf>,
    main_worktree: Option<PathBuf>,
    worktrees_path: Option<PathBuf>,
}

impl ManagedRootMeta {
    fn read(common_dir: &Path) -> Option<ManagedRootMeta> {
        let p = common_dir.join(".wrt").join("state.json");
        let b = fs::read(&p).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
        let root = v.get("root")?.as_object()?;
        if root.get("layout")?.as_str()? != crate::state::LAYOUT_MANAGED_ROOT {
            return None;
        }

        Some(ManagedRootMeta {
            managed_root: path_field(root, "managedRoot"),
            main_worktree: path_field(root, "mainWorktree"),
            worktrees_path: path_field(root, "worktreesPath"),
        })
    }
}

fn path_field(root: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<PathBuf> {
    root.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn repo_from_meta(
    meta: ManagedRootMeta,
    common_dir: PathBuf,
    invocation_root: Option<PathBuf>,
) -> Repo {
    let managed_root = meta
        .managed_root
        .or_else(|| common_dir.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let main_worktree = meta
        .main_worktree
        .unwrap_or_else(|| managed_root.join("main"));
    let worktree_parent = meta.worktrees_path.unwrap_or_else(|| managed_root.clone());
    Repo::new(
        managed_root,
        common_dir,
        main_worktree,
        worktree_parent,
        invocation_root,
    )
}

fn command_root(
    common_dir: &Path,
    managed_root: &Path,
    main_worktree: &Path,
    invocation_root: Option<&Path>,
) -> PathBuf {
    if main_worktree.is_dir() {
        return main_worktree.to_path_buf();
    }

    if let Some(invocation_root) = invocation_root {
        if invocation_root.is_dir() {
            return invocation_root.to_path_buf();
        }
    }

    first_linked_worktree(common_dir, managed_root).unwrap_or_else(|| managed_root.to_path_buf())
}

fn first_linked_worktree(common_dir: &Path, managed_root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(common_dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut path: Option<PathBuf> = None;
    let mut bare = false;

    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !bare {
                if let Some(path) = path.take() {
                    if path != managed_root && path.is_dir() {
                        return Some(path);
                    }
                }
            }
            path = None;
            bare = false;
            continue;
        }

        if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if line == "bare" {
            bare = true;
        }
    }

    None
}
