use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

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
        if let Some((prefix, base, tail)) = project_id_parts(line) {
            let suffix = sanitize_suffix(worktree_name);

            // Avoid double-suffixing if re-run.
            let mut want = base.to_string();
            if !suffix.is_empty() && !base.ends_with(&format!("-{suffix}")) {
                want = format!("{base}-{suffix}");
            }

            if want != base {
                *line = format!("{prefix}\"{want}\"{tail}");
                changed = true;
            }
            continue;
        }

        if let Some(nline) = patch_port_line(line, offset)? {
            if nline != *line {
                *line = nline;
                changed = true;
            }
            continue;
        }

        if line.contains("http://") || line.contains("https://") {
            let nline = patch_local_url_ports(line, offset);
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

fn project_id_parts(line: &str) -> Option<(&str, &str, &str)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("project_id") {
        return None;
    }

    let key_start = line.len() - trimmed.len();
    let after_key = key_start + "project_id".len();
    let rest = &line[after_key..];
    let eq_pos = rest.find('=')?;
    if !rest[..eq_pos].trim().is_empty() {
        return None;
    }

    let value_start = after_key + eq_pos + 1;
    let after_eq = &line[value_start..];
    let quote_offset = after_eq.find('"')?;
    if !after_eq[..quote_offset].trim().is_empty() {
        return None;
    }

    let open = value_start + quote_offset;
    let close = line[open + 1..].find('"')? + open + 1;
    let tail = &line[close + 1..];
    if !tail.trim_start().starts_with('#') && !tail.trim().is_empty() {
        return None;
    }

    Some((&line[..open], &line[open + 1..close], tail))
}

fn patch_port_line(line: &str, offset: i32) -> Result<Option<String>> {
    let trimmed = line.trim_start();
    let Some(key) = ["port", "shadow_port", "smtp_port", "pop3_port"]
        .iter()
        .find(|key| trimmed.starts_with(**key))
    else {
        return Ok(None);
    };

    let key_start = line.len() - trimmed.len();
    let after_key = key_start + key.len();
    let rest = &line[after_key..];
    let eq_pos = match rest.find('=') {
        Some(pos) if rest[..pos].trim().is_empty() => pos,
        _ => return Ok(None),
    };

    let value_start = after_key + eq_pos + 1;
    let after_eq = &line[value_start..];
    let digit_offset = after_eq.find(|ch: char| ch.is_ascii_digit());
    let Some(digit_offset) = digit_offset else {
        return Ok(None);
    };
    if !after_eq[..digit_offset].trim().is_empty() {
        return Ok(None);
    }

    let digits_start = value_start + digit_offset;
    let digits_len = line[digits_start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    let digits_end = digits_start + digits_len;
    let tail = &line[digits_end..];
    if !tail.trim_start().starts_with('#') && !tail.trim().is_empty() {
        return Ok(None);
    }

    let n: i32 = line[digits_start..digits_end].parse().unwrap_or(0);
    if n == 0 {
        return Ok(Some(line.to_string()));
    }

    let n2 = n + offset;
    if !(1..=65535).contains(&n2) {
        return Err(anyhow!("port out of range after offset: {n} -> {n2}"));
    }
    Ok(Some(format!("{}{}{}", &line[..digits_start], n2, tail)))
}

fn patch_local_url_ports(line: &str, offset: i32) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;

    while i < line.len() {
        let rest = &line[i..];
        let Some(prefix_len) = local_url_prefix(rest) else {
            out.push_str(&line[i..]);
            break;
        };

        out.push_str(&rest[..prefix_len]);
        let port_start = i + prefix_len;
        let port_len = line[port_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .map(char::len_utf8)
            .sum::<usize>();
        if port_len == 0 {
            i = port_start;
            continue;
        }

        let port_end = port_start + port_len;
        let port: i32 = line[port_start..port_end].parse().unwrap_or(0);
        let p2 = port + offset;
        if (1..=65535).contains(&p2) {
            out.push_str(&p2.to_string());
        } else {
            out.push_str(&line[port_start..port_end]);
        }
        i = port_end;
    }

    out
}

fn local_url_prefix(s: &str) -> Option<usize> {
    const PREFIXES: [&str; 4] = [
        "http://localhost:",
        "https://localhost:",
        "http://127.0.0.1:",
        "https://127.0.0.1:",
    ];

    let (idx, prefix) = PREFIXES
        .iter()
        .filter_map(|prefix| s.find(prefix).map(|idx| (idx, *prefix)))
        .min_by_key(|(idx, _)| *idx)?;
    Some(idx + prefix.len())
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
    fn sanitize_suffix_edge_cases() {
        assert_eq!(sanitize_suffix("--a--b--"), "a-b");
        assert_eq!(sanitize_suffix(""), "");
        assert_eq!(sanitize_suffix("café"), "caf");
        assert_eq!(sanitize_suffix("   "), "");
        assert_eq!(sanitize_suffix("a_b_c"), "a-b-c");
        assert_eq!(sanitize_suffix("ABC123"), "abc123");
        assert_eq!(sanitize_suffix("---"), "");
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

    #[test]
    fn patch_config_rejects_port_overflow() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(&p, "port = 65500\n").unwrap();

        let result = patch_config(td.path(), "test", 100);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("port out of range"),
            "expected port out of range error, got: {err}"
        );
        assert!(err.contains("65500") && err.contains("65600"));
    }

    #[test]
    fn patch_config_port_at_boundary() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(&p, "port = 65435\n").unwrap();

        patch_config(td.path(), "test", 100).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(
            out.contains("port = 65535"),
            "expected port = 65535, got: {out}"
        );
    }

    #[test]
    fn patch_config_project_id_is_idempotent() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(&p, "project_id = \"myproj\"\n").unwrap();

        patch_config(td.path(), "wt1", 0).unwrap();
        let after_first = fs::read_to_string(&p).unwrap();

        patch_config(td.path(), "wt1", 0).unwrap();
        let after_second = fs::read_to_string(&p).unwrap();

        assert_eq!(
            after_first, after_second,
            "second run should not change project_id"
        );
        assert!(after_first.contains("project_id = \"myproj-wt1\""));
    }

    #[test]
    fn patch_config_no_change_when_already_suffixed() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(&p, "project_id = \"myproj-wt1\"\n").unwrap();

        patch_config(td.path(), "wt1", 0).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(
            out.contains("project_id = \"myproj-wt1\""),
            "should not double-suffix: {out}"
        );
        assert!(!out.contains("myproj-wt1-wt1"));
    }

    #[test]
    fn patch_config_handles_all_port_types() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(
            &p,
            r#"port = 5432
shadow_port = 5433
smtp_port = 2500
pop3_port = 1100
"#,
        )
        .unwrap();

        patch_config(td.path(), "test", 100).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("port = 5532"), "port not offset: {out}");
        assert!(
            out.contains("shadow_port = 5533"),
            "shadow_port not offset: {out}"
        );
        assert!(
            out.contains("smtp_port = 2600"),
            "smtp_port not offset: {out}"
        );
        assert!(
            out.contains("pop3_port = 1200"),
            "pop3_port not offset: {out}"
        );
    }

    #[test]
    fn patch_config_preserves_comments() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(&p, "port = 5432 # database port\n").unwrap();

        patch_config(td.path(), "test", 100).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(
            out.contains("port = 5532 # database port"),
            "comment not preserved: {out}"
        );
    }

    #[test]
    fn patch_config_errors_on_missing_file() {
        let td = TempDir::new().unwrap();
        let result = patch_config(td.path(), "test", 100);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("read") || err.contains("config.toml") || err.contains("No such file"),
            "expected file not found error, got: {err}"
        );
    }
}
