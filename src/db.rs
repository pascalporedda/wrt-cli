use crate::project::ProjectConfig;
use crate::supabase;
use std::path::Path;

pub fn has_supabase_seed_or_migrations(root: &Path, target: &supabase::Target) -> bool {
    let sb = target.workdir(root).join("supabase");
    sb.join("seed.sql").exists() || sb.join("migrations").is_dir()
}

pub fn command(
    config: Option<&ProjectConfig>,
    wt_path: &Path,
    supabase_target: Option<&supabase::Target>,
    op: &str,
) -> (Option<String>, Option<Vec<String>>) {
    let mut kind = None;
    let mut cmd = None;

    if let Some(config) = config {
        cmd = match op {
            "reset" => config.commands().db_reset(),
            "seed" => config.commands().db_seed(),
            "migrate" => config.commands().db_migrate(),
            _ => None,
        }
        .map(|command| command.argv().to_vec());
        if supabase_target.is_some()
            && cmd
                .as_ref()
                .and_then(|argv| argv.first())
                .is_some_and(|program| program == "supabase")
        {
            kind = Some("supabase".to_string());
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
