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
| **Worktree orchestration** | Creates feature worktrees as siblings of `main` inside a managed root |
| **Managed roots** | Can clone/bootstrap `<root>/.git` as a bare common repo with `<root>/main` and `<root>/<feature>` checkouts |
| **Port block reservation** | Allocates a unique `WRT_PORT_BLOCK` per worktree (offset = `block * 100`) |
| **Shell-friendly env** | Writes `.wrt.env` into each worktree and can print `export ...` lines for your shell |
| **Supabase sharing + isolation** | Starts one main stack, lets features share it or create isolated stacks, and supports nested config paths |
| **Run inside worktree** | `wrt run <name> -- ...` runs a command in that worktree with `WRT_*` set |
| **State tracking** | Tracks worktrees in `<git-common-dir>/.wrt/state.json` and can prune missing entries |
| **Repo discovery (optional)** | `wrt init` can call the Codex CLI to generate managed-root `.wrt.json` conventions |

---

## Quick Start

```bash
# install locally
cargo install --path .

# clone into a managed root
wrt clone git@github.com:org/app.git --install false
cd app

# optional: generates shared .wrt.json via Codex at the managed root
cd main
wrt init
cd ..

wrt new a/gpt/login-timeout

# jump into it
cd "$(wrt path a-gpt-login-timeout)"

# load the worktree's reserved port block
eval "$(wrt env)"
echo "$WRT_PORT_OFFSET"
```

Managed-root layout:

```bash
wrt clone git@github.com:org/app.git --install false
cd app
wrt new a/gpt/login-timeout
cd "$(wrt path a-gpt-login-timeout)"
```

## Shell Integration

1. Zsh completions (manual `fpath`)
```zsh
mkdir -p ~/.zsh/completions
wrt completions zsh > ~/.zsh/completions/_wrt
echo 'fpath=(~/.zsh/completions $fpath)\nautoload -Uz compinit && compinit' >> ~/.zshrc
```
Restart your shell or `exec zsh`.

The completion script suggests tracked worktree names for commands such as
`wrt remove`, and local or fetched remote branch names for `wrt add`/`wrt new`.
Remote names are shown without their remote prefix (for example,
`feature/login` instead of `origin/feature/login`).

2. Oh My Zsh plugin
```zsh
mkdir -p ~/.oh-my-zsh/custom/plugins/wrt
wrt completions zsh > ~/.oh-my-zsh/custom/plugins/wrt/_wrt
cat > ~/.oh-my-zsh/custom/plugins/wrt/wrt.plugin.zsh <<'EOF'
fpath=(${0:A:h} $fpath)
EOF
```
Add `wrt` to `plugins=(...)` in `~/.zshrc`, then restart your shell.
When the Oh My Zsh plugin is enabled, regenerate the plugin's `_wrt` file after
upgrading `wrt`; updating `~/.zsh/completions/_wrt` does not update the active
plugin copy.

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
wrt clone <git-repo-url> [--root <dir>] [--main <branch>] [--install auto|true|false] [--supabase auto|true|false] [--supabase-config <path>] [--db auto|true|false]
wrt root init <source> --root <dir> [--main <branch>] [--install auto|true|false] [--supabase auto|true|false] [--supabase-config <path>] [--db auto|true|false]
wrt root status
wrt new <name> [--from <ref>] [--branch <branch>] [--install auto|true|false] [--supabase auto|shared|isolated|none] [--supabase-config <path>] [--db auto|true|false] [--cd]
wrt add <name> [--from <ref>] [--branch <branch>] [--install auto|true|false] [--supabase auto|shared|isolated|none] [--supabase-config <path>] [--db auto|true|false] [--cd]
wrt db [<name>] reset|seed|migrate [--print]
wrt ls
wrt path <name>
wrt env [<name>]
wrt rm <name> [--force] [--delete-branch]
wrt remove <name> [--force] [--delete-branch]
wrt prune
wrt housekeeping [--apply]
wrt run <name> -- <command> [args...]
wrt completions zsh
```

Examples:

```bash
# create from a ref (default is HEAD)
wrt new perf/agent-01 --from origin/main
wrt add perf/agent-02 --from origin/main

# create a managed root with sibling worktrees
wrt clone git@github.com:org/app.git --install false
cd app
wrt new perf/agent-01

# create a managed root from an existing local checkout
wrt root init . --root ../my-repo-wrt --install false
cd ../my-repo-wrt
wrt root status
wrt new perf/agent-01

# create and jump into it (shell integration)
eval "$(wrt new a/gpt/login-timeout --cd --install false)"

# keep the directory slugged but force a branch name
wrt new "Agent 02: API cleanup" --branch agent/api-cleanup

# explicitly create an isolated feature stack
wrt new x --supabase isolated

# explicitly skip Supabase for this worktree
wrt new docs-only --supabase none

# persist a nested Supabase project path for main and future worktrees
wrt clone git@github.com:org/monorepo.git --supabase-config apps/api/supabase/config.toml

# remove worktree (and optionally the branch ref)
wrt rm x --force
wrt remove x --force
wrt rm x --force --delete-branch

# prune stale state entries after manual deletions
wrt prune

# dry-run branch cleanup; add --apply to delete candidates
wrt housekeeping
```

---

## How It Works

- **Worktree paths**
  - `wrt new <name>` creates the slug as a sibling of `main` (example: `<root>/a-gpt-fix-login-timeout`)
  - `wrt clone <source>` derives `<root>` from the repo URL, then creates `<root>/.git` and `<root>/main`
  - `wrt root init <source> --root <dir>` creates `<dir>/.git` as the bare common repo and `<dir>/main` as block `0`
  - branch names keep slashes, but spaces are normalized to `-`
- **State**
  - tracked in `<git-common-dir>/.wrt/state.json` (usually `.git/.wrt/state.json`)
  - block `0` is reserved for the main workdir; first worktree usually gets block `1` => offset `100`
  - Supabase ownership and the repo-relative config path are persisted; credentials are not
- **Environment**
  - `.wrt.env`, `wrt env`, and `wrt run` share one environment resolver
  - generated vars include `WRT_ROOT`, `WRT_WORKTREE_PATH`, `WRT_MAIN_PATH`, `COMPOSE_PROJECT_NAME`, and discovered service port/url vars from checkout-local `.wrt.json` or the managed-root `.wrt.json`
  - Supabase values are read from `supabase status -o json` and synchronized into generated blocks in `.env` and the Supabase workdir's `.env`
  - if an `.env` is tracked, wrt leaves it unchanged and writes the generated block to `.env.local`
- **Git excludes**
  - `wrt` appends these to `.git/info/exclude` to reduce accidental commits:
    - `.env`
    - `.env.local`
    - `.wrt.env`
    - `.wrt.json`

<details>
<summary><b>Supabase patching details</b></summary>
<br>

When clone detects a Supabase config, it starts one unpatched stack from `main`. `wrt new`/`wrt add` asks whether to create an isolated stack; answering no reuses main. In non-interactive use, `auto` reuses main when available and otherwise disables Supabase.

- `--supabase shared` reuses the main stack
- `--supabase isolated` patches and starts a feature-owned stack
- `--supabase none` disables Supabase for that worktree
- `--supabase-config apps/api/supabase/config.toml` persists an alternative repo-relative path
- `true` and `false` remain aliases for `isolated` and `none` on feature commands

For isolated stacks:

- `project_id` gets a short suffix derived from the worktree name
- `port`, `shadow_port`, `smtp_port`, `pop3_port` are incremented by `WRT_PORT_OFFSET` across nested TOML sections
- `http://localhost:<port>` / `http://127.0.0.1:<port>` URL ports inside the config are also incremented
- the effective config path is marked `skip-worktree` in that worktree to reduce accidental commits

</details>

---

## Known Issues / Gotchas

- `wrt` commands operate only inside managed roots created by `wrt clone` or `wrt root init`
- `wrt run` must be invoked with `--` exactly like `wrt run <name> -- <command> ...` (otherwise it exits with code `2`)
- `wrt env` with no `<name>` only works when you run it from inside a tracked worktree (it infers from `cwd`)
- Clone fails its setup phase if a detected Supabase project cannot start or report status. The managed root and failed state remain available for recovery.
- Shared features never automatically reset the main database. Use explicit DB commands or `--db true` when destructive setup is intended.
- `wrt clone` / `wrt root init` clone committed Git state. They copy `.env` into `main` and `.wrt.json` into the managed root only when the source is a local directory and those files exist.
- Worktree name slugging is intentionally strict. If your `<name>` turns into an empty slug, it becomes `wrt`

---

## Tech Stack

- Rust (edition 2024, Rust 1.85+)
- clap (CLI parsing)
- serde / serde_json (state + discovery config)
- toml_edit (Supabase config patching)
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

`wrt init` can shell out to the Codex CLI to generate a shared `.wrt.json` at the managed root (useful if you want a shared "what services exist / which ports matter" contract for tooling). A checkout-local `.wrt.json` takes precedence when present.

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
just run help

# Run formatting, linting, build, and tests
just check

# Format
just fmt

# Lint
just lint

# Build
just build

# Tests
just test
```

---

<div align="center">

Built for people who keep 6 worktrees open and still want predictable ports.

</div>
