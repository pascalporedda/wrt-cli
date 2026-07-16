use crate::codex;
use crate::supabase;
use std::fs;
use std::path::Path;

pub fn has_supabase_seed_or_migrations(root: &Path, target: &supabase::Target) -> bool {
    let sb = target.workdir(root).join("supabase");
    sb.join("seed.sql").exists() || sb.join("migrations").is_dir()
}

pub fn command(
    config_root: &Path,
    wt_path: &Path,
    supabase_target: Option<&supabase::Target>,
    op: &str,
) -> (Option<String>, Option<Vec<String>>) {
    let mut kind = None;
    let mut cmd = None;

    let local_cfg = wt_path.join(".wrt.json");
    let fallback_cfg = config_root.join(".wrt.json");
    let cfg_path = if local_cfg.exists() {
        local_cfg
    } else {
        fallback_cfg
    };

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

    if cmd.is_none()
        && op == "reset"
        && supabase_target.is_some_and(|target| has_supabase_seed_or_migrations(wt_path, target))
    {
        kind = kind.or(Some("supabase".into()));
        cmd = Some(vec!["supabase".into(), "db".into(), "reset".into()]);
    }

    (kind, cmd)
}
