<div align="center">

# wrt

### git worktrees for parallel (agentic) workflows

![Rust](https://img.shields.io/badge/rust-2021-b7410e.svg)
![CLI](https://img.shields.io/badge/type-CLI-222.svg)
![Git Worktree](https://img.shields.io/badge/git-worktree-f14e32.svg)

<p>
Spin up multiple local sandboxes of the same repo, without port collisions, without copy-pasting env vars, and without
accidentally committing your "agent #7" Supabase changes.
</p>

</div>

---

## Why I Built This

I like running multiple branches at once. Humans do it, agents do it, and modern dev stacks absolutely hate it.

The pain is always the same:

- you create a second `git worktree`
- you run `dev`
- everything collides on ports, containers, project ids, and "helpful" config files

**The goal:** a tiny tool that turns "parallel work" into a one-liner.

**The vibe:** keep it boring and deterministic. When it touches your repo, it should be obvious what happened.

---

## What It Does

| | |
|---|---|
| **Worktree orchestration** | Creates worktrees under `<repo>/.worktrees/<name>` with a matching branch |
| **Port block reservation** | Allocates a unique `WRT_PORT_BLOCK` per worktree (offset = `block * 100`) |
| **Shell-friendly env** | Writes `.wrt.env` into each worktree and can print `export ...` lines for your shell |
| **Supabase isolation** | Patches `supabase/config.toml` (project_id suffix + port offsets + localhost URLs) and sets `skip-worktree` |
| **Run inside worktree** | `wrt run <name> -- ...` runs a command in that worktree with `WRT_*` set |
| **State tracking** | Tracks worktrees in `<git-common-dir>/.wrt/state.json` and can prune missing entries |
| **Repo discovery (optional)** | `wrt init` can call the Codex CLI to generate `.wrt.json` for repo-local conventions |

---

## Quick Start

```bash
# install locally
cargo install --path .

# in any git repo
wrt init            # optional: generates .wrt.json via Codex (see below)
wrt new a/gpt/login-timeout

# jump into it
cd "$(wrt path a-gpt-login-timeout)"

# load the worktree's reserved port block
eval "$(wrt env)"
echo "$WRT_PORT_OFFSET"
```

Zsh convenience wrapper (auto-`cd` on `wrt new`):

```zsh
wrt() {
  if [[ "$1" == "new" ]]; then
    eval "$(command wrt "$@" --cd)"
  else
    command wrt "$@"
  fi
}
```

Run a command inside the worktree (without `cd`):

```bash
wrt run a-gpt-login-timeout -- sh -lc 'echo $WRT_NAME && env | rg ^WRT_'
```

---

## Commands

```text
wrt init [--force] [--print] [--model <codex-model>]
wrt new <name> [--from <ref>] [--branch <branch>] [--install auto|true|false] [--supabase auto|true|false] [--db auto|true|false] [--cd]
wrt ls
wrt path <name>
wrt env [<name>]
wrt rm <name> [--force] [--delete-branch]
wrt prune
wrt run <name> -- <command> [args...]
```

Examples:

```bash
# create from a ref (default is HEAD)
wrt new perf/agent-01 --from origin/main

# create and jump into it (shell integration)
eval "$(wrt new a/gpt/login-timeout --cd --install false --supabase false)"

# keep the directory slugged but force a branch name
wrt new "Agent 02: API cleanup" --branch agent/api-cleanup

# skip dependency install and supabase
wrt new x --install false --supabase false

# remove worktree (and optionally the branch ref)
wrt rm x --force
wrt rm x --force --delete-branch

# prune stale state entries after manual deletions
wrt prune
```

---

## How It Works

- **Worktree paths**
  - `wrt new <name>` slugs the directory name (example: `a/gpt/fix-login-timeout` -> `.worktrees/a-gpt-fix-login-timeout`)
  - branch names keep slashes, but spaces are normalized to `-`
- **State**
  - tracked in `<git-common-dir>/.wrt/state.json` (usually `.git/.wrt/state.json`)
  - block `0` is reserved for the main workdir; first worktree usually gets block `1` => offset `100`
- **Git excludes**
  - `wrt` appends these to `.git/info/exclude` to reduce accidental commits:
    - `.worktrees/`
    - `.wrt.env`
    - `.wrt.json`

<details>
<summary><b>Supabase patching details</b></summary>
<br>

If `supabase/config.toml` exists inside the worktree, `wrt` can patch it for isolation:

- `project_id` gets a short suffix derived from the worktree name
- `port`, `shadow_port`, `smtp_port`, `pop3_port` are incremented by `WRT_PORT_OFFSET`
- `http://localhost:<port>` / `http://127.0.0.1:<port>` URL ports inside the config are also incremented
- `supabase/config.toml` is marked `skip-worktree` in that worktree to reduce accidental commits

</details>

---

## Known Issues / Gotchas

- `wrt run` must be invoked with `--` exactly like `wrt run <name> -- <command> ...` (otherwise it exits with code `2`)
- `wrt env` with no `<name>` only works when you run it from inside a tracked worktree (it infers from `cwd`)
- `wrt new --supabase auto` patches config if it sees `supabase/config.toml`, but it only runs `supabase start` if the Supabase CLI exists in `PATH`
- Worktree name slugging is intentionally strict. If your `<name>` turns into an empty slug, it becomes `wrt`

---

## Tech Stack

- Rust (edition 2021)
- clap (CLI parsing)
- serde / serde_json (state + discovery config)
- regex (Supabase config patching)
- chrono (timestamps)

<details>
<summary><b>Project structure</b></summary>
<br>

```text
src/        Rust CLI entrypoint + internal modules (git/worktree/state/supabase/codex/pm/ui)
assets/     embedded prompt + JSON schema used by wrt init
tests/      integration tests (temp git repos)
```

</details>

---

## Codex Discovery (`wrt init`)

`wrt init` can shell out to the Codex CLI to generate a repo-local `.wrt.json` (useful if you want a shared "what services exist / which ports matter" contract for tooling).

Offline testing:

```bash
# Make init read a pre-generated JSON file instead of calling codex
export WRT_CODEX_MOCK_OUTPUT=/path/to/out.json
wrt init --print
```

---

## Development

```bash
# Run from source
cargo run -- help

# Tests
cargo test

# Format
cargo fmt

# Lint
cargo clippy --all-targets -- -D warnings
```

---

<div align="center">

Built for people who keep 6 worktrees open and still want predictable ports.

</div>
