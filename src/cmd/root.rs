use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::cmd::new::{setup_existing_worktree, SetupModes};
use crate::gitx;
use crate::state::{Allocation, RootState, State, LAYOUT_MANAGED_ROOT};
use crate::supabase;
use crate::ui;
use crate::util::run_cmd;
use crate::worktree;

pub struct RootInitOpts<'a> {
    pub source: &'a str,
    pub root: &'a str,
    pub main: Option<&'a str>,
    pub install_mode: &'a str,
    pub sb_mode: &'a str,
    pub db_mode: &'a str,
}

pub struct CloneOpts<'a> {
    pub source: &'a str,
    pub root: Option<&'a str>,
    pub main: Option<&'a str>,
    pub install_mode: &'a str,
    pub sb_mode: &'a str,
    pub db_mode: &'a str,
}

pub fn cmd_clone(log: &ui::Logger, opts: CloneOpts<'_>) -> Result<i32> {
    let root = opts
        .root
        .map(str::to_string)
        .unwrap_or_else(|| default_clone_root(opts.source));
    let init = RootInitOpts {
        source: opts.source,
        root: &root,
        main: opts.main,
        install_mode: opts.install_mode,
        sb_mode: opts.sb_mode,
        db_mode: opts.db_mode,
    };
    cmd_root_init(log, init)
}

pub fn cmd_root_init(log: &ui::Logger, opts: RootInitOpts<'_>) -> Result<i32> {
    let root = std::env::current_dir()?.join(opts.root);
    let git_dir = root.join(".git");
    let main_path = root.join("main");

    if git_dir.exists() {
        log.errorf(&format!("{} already exists", git_dir.display()));
        return Ok(2);
    }
    if root.exists() && fs::read_dir(&root)?.next().is_some() {
        log.errorf(&format!("{} is not empty", root.display()));
        return Ok(2);
    }

    fs::create_dir_all(&root).with_context(|| format!("mkdir {}", root.display()))?;

    log.infof(&format!(
        "cloning bare repo: {} -> {}",
        opts.source,
        git_dir.display()
    ));
    let git_dir_arg = git_dir.to_string_lossy().to_string();
    if let Err(e) = run_cmd(
        Path::new("."),
        "git",
        &["clone", "--bare", opts.source, &git_dir_arg],
    ) {
        log.errorf(&format!("git clone --bare failed: {e}"));
        return Ok(1);
    }

    let branch = match opts.main.map(str::trim).filter(|s| !s.is_empty()) {
        Some(branch) => worktree::normalize_branch(branch),
        None => default_branch(&git_dir).unwrap_or_else(|| "main".to_string()),
    };

    log.infof(&format!(
        "creating main worktree: {} ({branch})",
        main_path.display()
    ));
    let main_path_arg = main_path.to_string_lossy().to_string();
    let add_args = [
        "--git-dir",
        git_dir_arg.as_str(),
        "worktree",
        "add",
        main_path_arg.as_str(),
        branch.as_str(),
    ];
    if let Err(e) = run_cmd(Path::new("."), "git", &add_args) {
        log.errorf(&format!("git worktree add main failed: {e}"));
        return Ok(1);
    }

    copy_source_overlays(opts.source, &main_path);

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let alloc = Allocation {
        name: "main".to_string(),
        branch: branch.clone(),
        path: main_path.to_string_lossy().to_string(),
        block: 0,
        offset: 0,
        status: "creating".to_string(),
        created_at: created_at.clone(),
    };

    let mut st = State::empty();
    st.root = Some(RootState {
        layout: LAYOUT_MANAGED_ROOT.to_string(),
        managed_root: root.to_string_lossy().to_string(),
        git_common_dir: git_dir.to_string_lossy().to_string(),
        main_worktree: main_path.to_string_lossy().to_string(),
        worktrees_path: root.to_string_lossy().to_string(),
        created_at,
    });
    st.allocations.insert("main".to_string(), alloc.clone());
    st.save(&git_dir)?;
    let _ = gitx::ensure_info_exclude(&git_dir, &[".wrt.env", ".wrt.json"]);

    let repo = gitx::Repo::new(
        root.clone(),
        git_dir.clone(),
        main_path.clone(),
        root.clone(),
        Some(main_path.clone()),
    );
    let modes = SetupModes {
        install_mode: opts.install_mode,
        sb_mode: opts.sb_mode,
        db_mode: opts.db_mode,
    };

    if let Err(e) = setup_existing_worktree(log, &repo, &alloc, &main_path, modes) {
        if let Some(a) = st.allocations.get_mut("main") {
            a.status = "failed".to_string();
        }
        let _ = st.save(&git_dir);
        log.errorf(&format!("main setup failed: {e}"));
        return Ok(1);
    }

    if let Some(a) = st.allocations.get_mut("main") {
        a.status = "active".to_string();
    }
    st.save(&git_dir)?;

    log.infof(&format!("managed root ready: {}", root.display()));
    Ok(0)
}

pub fn cmd_root_status(log: &ui::Logger, repo: &gitx::Repo, st: &State) -> Result<i32> {
    let Some(root) = &st.root else {
        log.errorf("current repository is not a wrt managed root");
        return Ok(2);
    };
    if root.layout != LAYOUT_MANAGED_ROOT {
        log.errorf("current repository is not a wrt managed root");
        return Ok(2);
    }

    println!("layout: {LAYOUT_MANAGED_ROOT}");
    println!("managed root: {}", root.managed_root);
    println!("git common dir: {}", root.git_common_dir);
    println!("main worktree: {}", root.main_worktree);
    println!("worktree parent: {}", root.worktrees_path);
    if let Some(invocation) = &repo.invocation_root {
        println!("invocation worktree: {}", invocation.display());
    } else {
        println!("invocation worktree: (managed root)");
    }
    println!("tracked worktrees: {}", st.allocations.len());

    let main_path = Path::new(&root.main_worktree);
    let supabase_status = if supabase::has_config(main_path) {
        "patched"
    } else {
        "none"
    };
    println!("supabase config: {supabase_status}");
    Ok(0)
}

fn default_clone_root(source: &str) -> String {
    if source.trim() == "." {
        if let Ok(cwd) = std::env::current_dir() {
            if let Some(name) = cwd.file_name().and_then(|s| s.to_str()) {
                return strip_git_suffix(name).to_string();
            }
        }
    }

    let source = source
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let name = source
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or("repo");
    let name = strip_git_suffix(name);
    if name.is_empty() {
        "repo".to_string()
    } else {
        name.to_string()
    }
}

fn strip_git_suffix(name: &str) -> &str {
    name.strip_suffix(".git").unwrap_or(name)
}

fn default_branch(git_dir: &Path) -> Option<String> {
    git_dir_out(git_dir, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            git_dir_out(git_dir, &["branch", "--format=%(refname:short)"])
                .ok()
                .and_then(|s| {
                    s.lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty())
                        .map(str::to_string)
                })
        })
}

fn copy_source_overlays(source: &str, main_path: &Path) {
    let source_path = Path::new(source);
    let source_path = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(source_path),
            Err(_) => return,
        }
    };
    if !source_path.is_dir() {
        return;
    }
    for file_name in [".env", ".wrt.json"] {
        let src = source_path.join(file_name);
        let dst = main_path.join(file_name);
        if src.is_file() && !dst.exists() {
            let _ = fs::copy(src, dst);
        }
    }
}

fn git_dir_out(git_dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        return Err(anyhow!("git command failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_root_derives_git_like_directory_names() {
        assert_eq!(default_clone_root("https://github.com/org/app.git"), "app");
        assert_eq!(default_clone_root("git@github.com:org/app.git"), "app");
        assert_eq!(default_clone_root("ssh://git@github.com/org/app"), "app");
        assert_eq!(
            default_clone_root("https://host/org/app.git?depth=1"),
            "app"
        );
        assert_eq!(default_clone_root(""), "repo");
    }
}
