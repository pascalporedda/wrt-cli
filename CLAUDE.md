# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build                              # Build
cargo run -- help                        # Run from source
cargo test                               # Run all tests
cargo test <test_name>                   # Run single test
cargo fmt                                # Format
cargo clippy --all-targets -- -D warnings # Lint
cargo install --path .                   # Install locally
```

## What This Is

`wrt` is a Rust CLI tool for managing git worktrees with port isolation. It's designed for parallel agentic workflows where multiple branches need to run simultaneously without port/container collisions.

Key features:
- Creates worktrees under `<repo>/.worktrees/<name>` with unique port blocks (offset = block * 100)
- Patches `supabase/config.toml` for isolation (project_id suffix, port offsets)
- Tracks state in `<git-common-dir>/.wrt/state.json`
- Can call Codex CLI (`wrt init`) to auto-discover repo config and generate `.wrt.json`

## Architecture

```
src/main.rs    - CLI entry point (clap), all command handlers (cmd_new, cmd_rm, etc.)
src/state.rs   - State persistence: Allocation struct, load/save to .wrt/state.json
src/worktree.rs - Git worktree operations: add, remove, slug/normalize helpers
src/gitx.rs    - Git utilities: repo detection, ensure_info_exclude
src/supabase.rs - Supabase config patching (ports, project_id, localhost URLs)
src/codex.rs   - Codex CLI integration for repo discovery (wrt init)
src/db.rs      - Database tooling detection (supabase, prisma, sqlx)
src/pm.rs      - Package manager detection (pnpm, npm, yarn, bun)
src/ui.rs      - Simple stderr logger
```

State lives in git's common dir (`.git/.wrt/state.json`) so it's shared across worktrees.

## Testing

Integration tests in `tests/cli.rs` create temporary git repos and exercise the full CLI. Tests use:
- `assert_cmd` for running the binary
- `tempfile` for isolated temp directories
- `WRT_CODEX_MOCK_OUTPUT` env var to mock Codex responses

## Key Conventions

- Worktree names are slugified (e.g., `a/gpt/fix-login-timeout` → `a-gpt-fix-login-timeout`)
- Branch names preserve slashes but normalize spaces to dashes
- Block 0 is reserved for main workdir; first worktree gets block 1 (offset 100)
- `.worktrees/`, `.wrt.env`, `.wrt.json` are auto-added to `.git/info/exclude`
