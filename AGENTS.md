# Repository Guidelines

## Project Structure & Module Organization

- `src/`: Rust CLI entrypoint and internal modules (git/worktree/state/supabase/codex/pm/ui).
- `assets/`: embedded prompt + JSON schema used by `wrt init`.
- `tests/`: integration tests (temp git repos).
- Runtime artifacts (do not commit): `.worktrees/<name>/`, `.wrt.env`, `.wrt.json`.

## Build, Test, and Development Commands

- `cargo install --path .`: build and install `wrt` into Cargo's bin dir.
- `cargo run -- help`: run from source without installing.
- `cargo test`: run the full test suite.
- `cargo fmt`: format.
- `cargo clippy --all-targets -- -D warnings`: lint.

## Coding Style & Naming Conventions

- Rust: follow standard Rust conventions; run `cargo fmt`.
- Prefer small modules with clear boundaries (`gitx`, `worktree`, `state`, `supabase`, `codex`, `pm`, `ui`).
- CLI flags use kebab-style as exposed to users (e.g. `--from`, `--delete-branch`).
- Worktree names and branches: `wrt new <name>` accepts slash-separated names (e.g. `a/gpt/fix-login-timeout`); the directory is slugged under `.worktrees/`.

## Testing Guidelines

- Prefer unit tests in-module for parsing/normalization.
- Use integration tests for filesystem mutations and git interactions (worktree creation/removal, `.wrt.env`, state persistence).
- Use temp dirs and init a temp git repo; avoid relying on global git state.

## Commit & Pull Request Guidelines

- Git history may be unavailable in this checkout (no `.git/`); use a consistent convention like `feat: ...`, `fix: ...`, `refactor: ...`, `test: ...`, `docs: ...`.
- PRs: describe what changed and why (including user-facing CLI behavior), include repro commands, and include `cargo test` output.

## Security & Configuration Tips

- `wrt init` shells out to the Codex CLI; for offline testing set `WRT_CODEX_MOCK_OUTPUT=/path/to/out.json`.
- `wrt` may patch `supabase/config.toml` to avoid port/container collisions; double-check local-only changes before committing.
