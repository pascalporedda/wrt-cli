use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::fs;
use std::path::Path;

fn re_port_assign() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(\s*(?:port|shadow_port|smtp_port|pop3_port)\s*=\s*)(\d+)(\s*(?:#.*)?)$")
            .expect("regex")
    })
}

fn re_project_id() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"^(\s*project_id\s*=\s*)"(.*)"(\s*(?:#.*)?)$"#).expect("regex"))
}

fn re_local_url_port() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"((?:https?://)(?:127\.0\.0\.1|localhost)):(\d+)").expect("regex")
    })
}

pub fn has_config(repo_root: &Path) -> bool {
    repo_root.join("supabase").join("config.toml").exists()
}

// patch_config updates supabase/config.toml inside the given worktree directory so multiple local
// supabase instances can run concurrently:
// - project_id gets a suffix derived from worktree name
// - port/shadow_port etc are incremented by offset
// - localhost URLs with explicit ports get the same offset
pub fn patch_config(worktree_root: &Path, worktree_name: &str, offset: i32) -> Result<()> {
    let p = worktree_root.join("supabase").join("config.toml");
    let b = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;

    let mut lines: Vec<String> = b.split('\n').map(|s| s.to_string()).collect();
    let mut changed = false;

    for line in &mut lines {
        if let Some(caps) = re_project_id().captures(line) {
            let base = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let suffix = sanitize_suffix(worktree_name);

            // Avoid double-suffixing if re-run.
            let mut want = base.to_string();
            if !suffix.is_empty() && !base.ends_with(&format!("-{suffix}")) {
                want = format!("{base}-{suffix}");
            }

            if want != base {
                let prefix = caps.get(1).unwrap().as_str();
                let tail = caps.get(3).unwrap().as_str();
                *line = format!("{prefix}\"{want}\"{tail}");
                changed = true;
            }
            continue;
        }

        if let Some(caps) = re_port_assign().captures(line) {
            let n: i32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            if n > 0 {
                let n2 = n + offset;
                if !(1..=65535).contains(&n2) {
                    return Err(anyhow!("port out of range after offset: {n} -> {n2}"));
                }
                if n2 != n {
                    let prefix = caps.get(1).unwrap().as_str();
                    let tail = caps.get(3).unwrap().as_str();
                    *line = format!("{prefix}{n2}{tail}");
                    changed = true;
                }
            }
            continue;
        }

        if line.contains("http://") || line.contains("https://") {
            let nline = re_local_url_port().replace_all(line, |caps: &regex::Captures| {
                let host = caps.get(1).unwrap().as_str();
                let port: i32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
                let p2 = port + offset;
                if !(1..=65535).contains(&p2) {
                    return format!("{host}:{port}");
                }
                format!("{host}:{p2}")
            });
            let nline = nline.to_string();
            if nline != *line {
                *line = nline;
                changed = true;
            }
        }
    }

    if !changed {
        return Ok(());
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }

    fs::write(&p, out.as_bytes()).with_context(|| format!("write {}", p.display()))?;
    Ok(())
}

fn sanitize_suffix(s: &str) -> String {
    let mut s = s.trim().to_lowercase();

    // Keep it short; docker resource names can get long fast.
    if s.len() > 24 {
        s.truncate(24);
    }

    // Replace anything non [a-z0-9-] with '-' and compress.
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if ok {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sanitize_suffix_limits_and_dashes() {
        assert_eq!(sanitize_suffix("A B C"), "a-b-c");
        assert!(sanitize_suffix("x".repeat(100).as_str()).len() <= 24);
    }

    #[test]
    fn patch_config_updates_ports_and_project_and_urls() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(
            &p,
            "project_id = \"myproj\"\nport = 5432\nauth_site_url = \"http://localhost:3000\"\n",
        )
        .unwrap();

        patch_config(td.path(), "a-gpt-fix", 200).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("project_id = \"myproj-a-gpt-fix\""));
        assert!(out.contains("port = 5632"));
        assert!(out.contains("http://localhost:3200"));
    }
}
