use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;
use toml_edit::{value, DocumentMut, InlineTable, Item, Table, Value};

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
    let mut doc = b
        .parse::<DocumentMut>()
        .with_context(|| format!("parse {}", p.display()))?;

    let mut changed = false;

    if let Some(project_id) = doc.get("project_id").and_then(Item::as_str) {
        let suffix = sanitize_suffix(worktree_name);
        let mut want = project_id.to_string();
        if !suffix.is_empty() && !project_id.ends_with(&format!("-{suffix}")) {
            want = format!("{project_id}-{suffix}");
        }
        if want != project_id {
            doc["project_id"] = value(want);
            changed = true;
        }
    }

    changed |= patch_table(doc.as_table_mut(), offset)?;

    if !changed {
        return Ok(());
    }

    let mut out = doc.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }

    fs::write(&p, out.as_bytes()).with_context(|| format!("write {}", p.display()))?;
    Ok(())
}

fn patch_table(table: &mut Table, offset: i32) -> Result<bool> {
    let mut changed = false;
    for (key, item) in table.iter_mut() {
        changed |= patch_item(Some(key.get()), item, offset)?;
    }
    Ok(changed)
}

fn patch_item(key: Option<&str>, item: &mut Item, offset: i32) -> Result<bool> {
    match item {
        Item::None => Ok(false),
        Item::Value(v) => patch_value(key, v, offset),
        Item::Table(t) => patch_table(t, offset),
        Item::ArrayOfTables(tables) => {
            let mut changed = false;
            for table in tables.iter_mut() {
                changed |= patch_table(table, offset)?;
            }
            Ok(changed)
        }
    }
}

fn patch_value(key: Option<&str>, value: &mut Value, offset: i32) -> Result<bool> {
    if key.map(is_port_key).unwrap_or(false) {
        if let Some(n) = value.as_integer() {
            if n == 0 {
                return Ok(false);
            }
            let n2 = n + i64::from(offset);
            if !(1..=65535).contains(&n2) {
                return Err(anyhow!("port out of range after offset: {n} -> {n2}"));
            }
            if n2 != n {
                replace_value_preserving_decor(value, Value::from(n2));
                return Ok(true);
            }
        }
    }

    if let Some(s) = value.as_str() {
        let nline = patch_local_url_ports(s, offset);
        if nline != s {
            replace_value_preserving_decor(value, Value::from(nline));
            return Ok(true);
        }
        return Ok(false);
    }

    match value {
        Value::Array(array) => {
            let mut changed = false;
            for value in array.iter_mut() {
                changed |= patch_value(None, value, offset)?;
            }
            Ok(changed)
        }
        Value::InlineTable(table) => patch_inline_table(table, offset),
        _ => Ok(false),
    }
}

fn patch_inline_table(table: &mut InlineTable, offset: i32) -> Result<bool> {
    let mut changed = false;
    for (key, value) in table.iter_mut() {
        changed |= patch_value(Some(key.get()), value, offset)?;
    }
    Ok(changed)
}

fn replace_value_preserving_decor(value: &mut Value, mut next: Value) {
    *next.decor_mut() = value.decor().clone();
    *value = next;
}

fn is_port_key(key: &str) -> bool {
    matches!(key, "port" | "shadow_port" | "smtp_port" | "pop3_port")
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
    fn patch_config_handles_nested_supabase_sections_and_url_arrays() {
        let td = TempDir::new().unwrap();
        let sbdir = td.path().join("supabase");
        fs::create_dir_all(&sbdir).unwrap();
        let p = sbdir.join("config.toml");
        fs::write(
            &p,
            r#"project_id = "myproj"

[api]
port = 54321

[db]
port = 54322
shadow_port = 54320

[studio]
port = 54323

[inbucket]
port = 54324
smtp_port = 54325
pop3_port = 54326

[auth]
site_url = "http://localhost:3000"
additional_redirect_urls = ["http://localhost:3001/callback", "https://127.0.0.1:3002/auth"]
"#,
        )
        .unwrap();

        patch_config(td.path(), "feature/test", 100).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("project_id = \"myproj-feature-test\""));
        assert!(out.contains("port = 54421"));
        assert!(out.contains("port = 54422"));
        assert!(out.contains("shadow_port = 54420"));
        assert!(out.contains("smtp_port = 54425"));
        assert!(out.contains("pop3_port = 54426"));
        assert!(out.contains("http://localhost:3100"));
        assert!(out.contains("http://localhost:3101/callback"));
        assert!(out.contains("https://127.0.0.1:3102/auth"));
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
