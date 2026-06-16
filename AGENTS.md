# Repository Notes

- Rust CLI code lives in `src/`, split by concern: `gitx`, `worktree`, `state`, `supabase`, `codex`, `pm`, `ui`.
- `assets/` contains the prompt and JSON schema embedded by `wrt init`.
- `tests/` contains integration tests that create temp git repos.
- Do not commit runtime artifacts: `.worktrees/<name>/`, `.wrt.env`, `.wrt.json`.

## Local Behavior

- `wrt new <name>` accepts slash-separated names like `a/gpt/fix-login-timeout`; the directory name is slugged under `.worktrees/`.
- `wrt init` shells out to the Codex CLI. For offline tests, set `WRT_CODEX_MOCK_OUTPUT=/path/to/out.json`.
- `wrt` may patch `supabase/config.toml` to avoid port/container collisions; review those local-only changes before committing.

## Checks

- Usual loop: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
