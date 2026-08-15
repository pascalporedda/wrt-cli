use anyhow::{Context, Result, anyhow};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value, value};

use crate::state::{Allocation, State, SupabaseAllocation};

pub const DEFAULT_CONFIG_PATH: &str = "supabase/config.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    config_path: PathBuf,
    workdir: PathBuf,
}

impl Target {
    pub fn from_config_path(path: &str) -> Result<Target> {
        let path = path.trim();
        if path.is_empty() {
            return Err(anyhow!("supabase config path is empty"));
        }

        let path = Path::new(path);
        if path.is_absolute() {
            return Err(anyhow!("supabase config path must be repo-relative"));
        }

        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(anyhow!(
                        "supabase config path must stay inside the worktree"
                    ));
                }
            }
        }

        if !normalized.ends_with(Path::new(DEFAULT_CONFIG_PATH)) {
            return Err(anyhow!(
                "supabase config path must end with {DEFAULT_CONFIG_PATH}"
            ));
        }

        let workdir = normalized
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        Ok(Target {
            config_path: normalized,
            workdir,
        })
    }

    pub fn config_path_string(&self) -> String {
        self.config_path.to_string_lossy().to_string()
    }

    pub fn absolute_config_path(&self, repo_root: &Path) -> PathBuf {
        repo_root.join(&self.config_path)
    }

    pub fn workdir(&self, repo_root: &Path) -> PathBuf {
        repo_root.join(&self.workdir)
    }

    pub fn relative_workdir(&self) -> &Path {
        &self.workdir
    }
}

pub fn resolve_target(
    repo_root: &Path,
    explicit: Option<&str>,
    stored: Option<&str>,
) -> Result<Target> {
    let configured = explicit
        .map(str::to_string)
        .or_else(|| stored.map(str::to_string))
        .or_else(|| discovery_config_path(repo_root))
        .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
    Target::from_config_path(&configured)
}

pub fn has_config(repo_root: &Path, target: &Target) -> bool {
    target.absolute_config_path(repo_root).is_file()
}

pub fn allocation_target<'a>(
    state: &'a State,
    allocation: &'a Allocation,
) -> Result<Option<(&'a Allocation, Target)>> {
    match &allocation.supabase {
        SupabaseAllocation::Owned { config_path, .. } => {
            Ok(Some((allocation, Target::from_config_path(config_path)?)))
        }
        SupabaseAllocation::Shared { owner } => {
            // Older state always used "main" as a logical primary-owner marker, even when the
            // managed root's primary checkout had another name such as "staging".
            let owner = if owner == "main" {
                state.primary_allocation().map(|(_, allocation)| allocation)
            } else {
                state.allocations.get(owner)
            }
            .ok_or_else(|| anyhow!("supabase owner worktree is missing: {owner}"))?;
            let SupabaseAllocation::Owned { config_path, .. } = &owner.supabase else {
                return Err(anyhow!(
                    "supabase owner {} does not own a running stack",
                    owner.name
                ));
            };
            Ok(Some((owner, Target::from_config_path(config_path)?)))
        }
        SupabaseAllocation::None => Ok(None),
    }
}

pub fn command_workdir(target: Option<&(&Allocation, Target)>, fallback: &Path) -> PathBuf {
    target
        .map(|(owner, target)| target.workdir(Path::new(&owner.path)))
        .unwrap_or_else(|| fallback.to_path_buf())
}

pub fn project_id(repo_root: &Path, target: &Target) -> Result<String> {
    let path = target.absolute_config_path(repo_root);
    let input = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let doc = input
        .parse::<DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    doc.get("project_id")
        .and_then(Item::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{} has no project_id", path.display()))
}

pub fn ensure_started(repo_root: &Path, target: &Target) -> Result<BTreeMap<String, String>> {
    if let Ok(status) = status_env(repo_root, target) {
        return Ok(status);
    }

    let workdir = target.workdir(repo_root);
    crate::util::run_cmd(&workdir, "supabase", &["start"])?;
    status_env(repo_root, target)
}

pub fn status_env(repo_root: &Path, target: &Target) -> Result<BTreeMap<String, String>> {
    let workdir = target.workdir(repo_root);
    let output = Command::new("supabase")
        .args(["status", "-o", "json"])
        .current_dir(&workdir)
        .output()
        .with_context(|| format!("run supabase status in {}", workdir.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            anyhow!("supabase status failed")
        } else {
            anyhow!("supabase status failed: {detail}")
        });
    }

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse `supabase status -o json` output")?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("supabase status JSON must be an object"))?;
    let mut vars = BTreeMap::new();
    for (key, value) in object {
        if let Some(value) = value.as_str() {
            vars.insert(key.clone(), value.to_string());
        }
    }
    if !vars.contains_key("API_URL") {
        return Err(anyhow!("supabase status did not return API_URL"));
    }
    Ok(vars)
}

pub fn stop(repo_root: &Path, target: &Target) -> Result<()> {
    let workdir = target.workdir(repo_root);
    crate::util::run_cmd(&workdir, "supabase", &["stop"])
}

fn discovery_config_path(repo_root: &Path) -> Option<String> {
    let input = fs::read_to_string(repo_root.join(".wrt.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&input).ok()?;
    value
        .get("supabase")?
        .get("config_path")?
        .as_str()
        .map(str::to_string)
}

// patch_config updates supabase/config.toml inside the given worktree directory so multiple local
// supabase instances can run concurrently:
// - project_id gets a suffix derived from worktree name
// - port/shadow_port etc are incremented by offset
// - localhost URLs with explicit ports get the same offset
pub fn patch_config(
    worktree_root: &Path,
    target: &Target,
    worktree_name: &str,
    offset: i32,
) -> Result<()> {
    let p = target.absolute_config_path(worktree_root);
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
    let s = s.trim().to_lowercase();
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

    let out = out.trim_matches('-').to_string();
    if out.len() <= 24 {
        return out;
    }

    let mut hash = 2_166_136_261_u32;
    for byte in out.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("{}-{hash:08x}", &out[..15])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn target() -> Target {
        Target::from_config_path(DEFAULT_CONFIG_PATH).unwrap()
    }

    #[test]
    fn target_accepts_nested_repo_relative_config() {
        let target = Target::from_config_path("apps/api/supabase/config.toml").unwrap();
        assert_eq!(target.config_path_string(), "apps/api/supabase/config.toml");
        assert_eq!(target.relative_workdir(), Path::new("apps/api"));
    }

    #[test]
    fn target_rejects_unsafe_or_nonstandard_paths() {
        assert!(Target::from_config_path("../supabase/config.toml").is_err());
        assert!(Target::from_config_path("/tmp/supabase/config.toml").is_err());
        assert!(Target::from_config_path("apps/api/config.toml").is_err());
    }

    #[test]
    fn sanitize_suffix_limits_and_dashes() {
        assert_eq!(sanitize_suffix("A B C"), "a-b-c");
        assert!(sanitize_suffix("x".repeat(100).as_str()).len() <= 24);
        assert_ne!(
            sanitize_suffix("same-very-long-worktree-prefix-one"),
            sanitize_suffix("same-very-long-worktree-prefix-two")
        );
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

        patch_config(td.path(), &target(), "a-gpt-fix", 200).unwrap();
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

        let result = patch_config(td.path(), &target(), "test", 100);
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

        patch_config(td.path(), &target(), "test", 100).unwrap();
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

        patch_config(td.path(), &target(), "wt1", 0).unwrap();
        let after_first = fs::read_to_string(&p).unwrap();

        patch_config(td.path(), &target(), "wt1", 0).unwrap();
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

        patch_config(td.path(), &target(), "wt1", 0).unwrap();
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

        patch_config(td.path(), &target(), "test", 100).unwrap();
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

        patch_config(td.path(), &target(), "feature/test", 100).unwrap();
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

        patch_config(td.path(), &target(), "test", 100).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        assert!(
            out.contains("port = 5532 # database port"),
            "comment not preserved: {out}"
        );
    }

    #[test]
    fn patch_config_errors_on_missing_file() {
        let td = TempDir::new().unwrap();
        let result = patch_config(td.path(), &target(), "test", 100);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("read") || err.contains("config.toml") || err.contains("No such file"),
            "expected file not found error, got: {err}"
        );
    }
}
