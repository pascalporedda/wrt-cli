use crate::codex;
use std::fs;
use std::path::Path;

pub fn has_supabase_seed_or_migrations(root: &Path) -> bool {
    let sb = root.join("supabase");
    sb.join("seed.sql").exists() || sb.join("migrations").is_dir()
}

pub fn command(
    repo_root: &Path,
    wt_path: &Path,
    op: &str,
) -> (Option<String>, Option<Vec<String>>) {
    let mut kind = None;
    let mut cmd = None;

    let cfg_path = repo_root.join(".wrt.json");
    if cfg_path.exists() {
        if let Ok(s) = fs::read_to_string(&cfg_path) {
            if let Ok(d) = serde_json::from_str::<codex::Discovery>(&s) {
                if d.database.detected {
                    kind = d.database.kind.clone();
                }
                cmd = match op {
                    "reset" => d.database.reset_command.clone(),
                    "seed" => d.database.seed_command.clone(),
                    "migrate" => d.database.migrate_command.clone(),
                    _ => None,
                };
            }
        }
    }

    if cmd.is_none() && op == "reset" && has_supabase_seed_or_migrations(wt_path) {
        kind = kind.or(Some("supabase".into()));
        cmd = Some(vec!["supabase".into(), "db".into(), "reset".into()]);
    }

    (kind, cmd)
}
