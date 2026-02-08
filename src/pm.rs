use std::path::Path;

pub fn has_project(root: &Path) -> bool {
    root.join("package.json").exists()
}

pub fn detect_install_command(root: &Path) -> Option<(String, Vec<String>)> {
    // Prefer lockfiles for reliability.
    if root.join("pnpm-lock.yaml").exists() {
        return Some(("pnpm".into(), vec!["install".into()]));
    }
    if root.join("package-lock.json").exists() {
        return Some(("npm".into(), vec!["ci".into()]));
    }
    if root.join("yarn.lock").exists() {
        return Some(("yarn".into(), vec!["install".into()]));
    }
    if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        return Some(("bun".into(), vec!["install".into()]));
    }
    if root.join("package.json").exists() {
        return Some(("npm".into(), vec!["install".into()]));
    }
    None
}
