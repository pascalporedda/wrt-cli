pub fn zsh_script() -> &'static str {
    include_str!("../assets/completions.zsh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsh_completes_worktrees_and_remote_branches() {
        let script = zsh_script();

        assert!(script.contains("case $line[1] in"));
        assert!(script.contains("case $line[2] in"));
        assert!(script.contains("'1:worktree:_wrt_worktrees'"));
        assert!(script.contains("$1 != \"(no\""));
        assert!(script.contains("'1:name:_wrt_branches'"));
        assert!(script.contains("path|setup)"));
        assert!(script.contains("env|doctor)"));
        assert!(script.contains("refs/heads refs/remotes"));
        assert!(script.contains("sub(\"^refs/remotes/[^/]+/\", \"\", name)"));
    }
}
