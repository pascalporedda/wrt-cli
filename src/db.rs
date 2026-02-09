use std::path::Path;

pub fn has_supabase_seed_or_migrations(root: &Path) -> bool {
    let sb = root.join("supabase");
    if !sb.is_dir() {
        return false;
    }
    if sb.join("seed.sql").exists() {
        return true;
    }
    if sb.join("migrations").is_dir() {
        return true;
    }
    false
}

pub fn has_prisma_schema(root: &Path) -> bool {
    root.join("prisma").join("schema.prisma").exists() || root.join("schema.prisma").exists()
}

pub fn has_sqlx_markers(root: &Path) -> bool {
    if root.join("sqlx-data.json").exists() {
        return true;
    }
    if root.join("migrations").is_dir() {
        // Heuristic: lots of tools use migrations/, but if we don't have better info, keep it weak.
        // We only use this for "kind" hints and logging.
        return true;
    }
    let cargo = root.join("Cargo.toml");
    if let Ok(s) = std::fs::read_to_string(cargo) {
        // Cheap substring scan; avoid TOML parsing deps.
        if s.contains("sqlx") {
            return true;
        }
    }
    false
}
