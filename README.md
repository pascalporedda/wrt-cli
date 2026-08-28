<div align="center">

# wrt

### git worktrees for parallel (agentic) workflows

![Rust](https://img.shields.io/badge/rust-2024-b7410e.svg)
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
| **Project port reservation** | Allocates a unique `WRT_PORT_BLOCK` per worktree and persists concrete host ports from `.wrt.json` |
| **Shell-friendly env** | Writes `.wrt.env` into each worktree and can print `export ...` lines for your shell |
| **Repository-owned setup and runtime** | Runs declared `setup`, `start`, `stop`, `status`, and database commands with the resolved WRT environment |
| **Compose port and container-name preflight** | Checks `docker compose config --format json` before a declared setup command and `wrt runtime start` |
| **Supabase sharing + isolation** | Starts one main stack, lets features share it or create isolated stacks, and supports nested config paths |
| **Run inside worktree** | `wrt run <name> -- ...` runs a command in that worktree with `WRT_*` set |
| **State tracking** | Tracks worktrees in `<git-common-dir>/.wrt/state.json` and can prune missing entries |
| **Repo discovery (optional)** | `wrt init` can call the Codex CLI to generate managed-root `.wrt.json` conventions for Supabase and non-Supabase projects |

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

eval "$(wrt env)"
echo "$WRT_PORT_OFFSET"
```

If version 2 declares runtime commands, manage them from the tracked worktree:

```bash
wrt runtime start
wrt runtime status
wrt runtime stop
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

## Project config

`wrt` reads `.wrt.json` from the checkout first. If the checkout does not have one, `wrt` falls back to the managed-root copy. Invalid checkout-local config fails the command. It does not silently fall back.

Version 2 is the project contract for non-Supabase stacks. It declares host-reachable ports, environment outputs, repository-owned commands, and optional Compose files for the isolation preflight.

This complete config reserves one host port and declares source-process runtime commands:

```json
{
  "version": 2,
  "port_stride": 100,
  "ports": [
    {
      "key": "api",
      "base_port": 3000,
      "outputs": [
        {"env": "API_PORT", "template": "{port}"},
        {"env": "API_URL", "template": "http://localhost:{port}"}
      ]
    }
  ],
  "commands": {
    "setup": null,
    "start": {"argv": ["sh", "scripts/runtime.sh", "start"], "cwd": null},
    "stop": {"argv": ["sh", "scripts/runtime.sh", "stop"], "cwd": null},
    "status": {"argv": ["sh", "scripts/runtime.sh", "status"], "cwd": null},
    "db_migrate": null,
    "db_seed": null,
    "db_reset": null
  },
  "compose": null,
  "supabase": null
}
```

An ELN-style project can reserve Postgres, Redis, API, and web ports from one config:

```json
{
  "version": 2,
  "port_stride": 100,
  "ports": [
    {
      "key": "postgres",
      "base_port": 5432,
      "outputs": [
        {"env": "POSTGRES_PORT", "template": "{port}"},
        {"env": "DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/core"},
        {"env": "AUTH_DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/auth"},
        {"env": "NOTIFICATION_DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/notification"}
      ]
    },
    {
      "key": "redis",
      "base_port": 6379,
      "outputs": [
        {"env": "REDIS_PORT", "template": "{port}"},
        {"env": "REDIS_URL", "template": "redis://localhost:{port}"}
      ]
    },
    {
      "key": "core-api",
      "base_port": 3000,
      "outputs": [
        {"env": "CORE_API_PORT", "template": "{port}"},
        {"env": "VITE_API_URL", "template": "http://localhost:{port}/api/v1"}
      ]
    },
    {
      "key": "web",
      "base_port": 5173,
      "outputs": [
        {"env": "WEB_PORT", "template": "{port}"}
      ]
    }
  ],
  "commands": {
    "setup": {"argv": ["pnpm", "run", "dev:setup"], "cwd": null},
    "start": {"argv": ["pnpm", "run", "dev:start"], "cwd": null},
    "stop": {"argv": ["pnpm", "run", "dev:stop"], "cwd": null},
    "status": {"argv": ["pnpm", "run", "dev:status"], "cwd": null},
    "db_migrate": {"argv": ["pnpm", "run", "db:migrate"], "cwd": null},
    "db_seed": {"argv": ["pnpm", "run", "db:seed"], "cwd": null},
    "db_reset": {"argv": ["pnpm", "run", "db:reset"], "cwd": null}
  },
  "compose": {
    "files": ["compose.yaml"]
  },
  "supabase": null
}
```

One Postgres port can produce several host URLs. Redis, API, and web ports stay separate. `wrt` persists each concrete port in managed-root state. The allocator locks state while it selects the full set, so concurrent `wrt new` commands cannot reserve the same port. An existing worktree keeps its ports after base-port edits. If the set of port keys changes, recreate the worktree.

Version 2 uses a strict shape. All six top-level fields and all seven command fields are required. Set unused fields to `null`. Unknown fields fail validation. Ports and `port_stride` must be in `1..=65535`. Port keys start with a lowercase letter and contain lowercase letters, digits, and single hyphens. Output names use letters, digits, and underscores, cannot start with a digit or `WRT_`, and must be unique. Templates contain one or more `{port}` placeholders and no other braces.

For block `n`, a candidate port is `base_port + n * port_stride`. The allocator can skip any block whose complete port set is invalid or already claimed. Only the selected concrete ports persist in state.

`argv` passes arguments directly without a shell. Use `{"argv":["sh","-lc","command | other"]}` only when shell syntax is required. `cwd` is relative to the worktree. It must exist and cannot resolve outside the worktree.

When `commands.setup` is present, `wrt clone`, `wrt root init`, and `wrt new` write `.wrt.env` and run only that command. Setup must preserve developer data and be safe to rerun after a partial failure. `wrt setup <name>` preserves the allocation and concrete ports, then resolves outputs and commands from the current config. The declared project setup path never invokes `db_reset` automatically.

`wrt runtime [<name>] [--worktree <name>] start|stop|status` runs the declared command with the resolved environment. If you run it inside a tracked worktree, `<name>` is optional. Use `--worktree` when a worktree is named `start`, `stop`, or `status`. `wrt` waits for the command to exit and returns its exit code. A repository that exposes `status` and `stop` must make `start` return after it launches the runtime in the background. Its `stop` command must succeed when the runtime is already stopped. A missing worktree, config, or command exits with code `2`. An unsafe Compose preflight exits with code `1` before `start` runs.

`wrt db [<name>] [--worktree <name>] reset|seed|migrate` runs the matching database command from `.wrt.json`, or the known Supabase command on the legacy path. Use `--worktree` when the worktree is named `reset`, `seed`, or `migrate`. `--print` prints the command without running it. Reset asks for confirmation on a terminal. In non-interactive use, reset requires `--yes`.

Set `supabase` to `null` when the project does not use Supabase. To declare Supabase, set it to `{"config_path":"supabase/config.toml"}`. When `commands.setup` is present, that command owns Supabase preparation and startup. The legacy setup path retains automatic shared or isolated Supabase behavior. Version 1 configs and the legacy Supabase-only config remain supported.

## Compose projects

Declare `compose.files` only when the repository already owns the Compose stack. List base and override files in command-line order. `wrt` does not rewrite Compose YAML. It renders the files twice with synthetic allocations and compares service-level published ports and explicit `container_name` values.

Use published host ports that depend on exported env vars:

```yaml
services:
  postgres:
    ports:
      - "${POSTGRES_PORT:-5432}:5432"
```

Do not keep fixed host ports such as `"5432:5432"`. Do not set fixed `container_name` values. Those settings make two worktrees collide even when `wrt` allocates different ports and project names.

Outputs such as `DATABASE_URL=...localhost:{port}` are for commands that run on the host. Containers must keep separate service-DNS values such as `postgresql://postgres:postgres@postgres:5432/core` and `redis://redis:6379`. A container cannot use the host's `localhost` URL to reach another container.

Let Compose derive network and volume names from `COMPOSE_PROJECT_NAME`. Do not set top-level resource `name` values that are shared across worktrees. Check host bind mounts, secrets, configs, and other global resources yourself. `wrt doctor` does not inspect them.

`wrt doctor [<name>]` reports render failures, malformed output, unsafe synthetic ports, fixed or duplicate host ports, fixed container names, and service output-shape changes. A declared `commands.setup` command and `wrt runtime [<name>] start` run the same check and block on any finding.

`wrt doctor` exits with code `0` for a safe render or when no Compose check is configured. It exits with code `1` for findings and code `2` for a missing or unknown worktree. Do not use its success alone to require that a project declares Compose files.

---

## Commands

```text
wrt init [--force] [--print] [--accept-commands] [--model <codex-model>]
wrt clone <git-repo-url> [--root <dir>] [--main <branch>] [--install auto|true|false] [--supabase auto|true|false] [--supabase-config <path>] [--db auto|true|false]
wrt root init <source> --root <dir> [--main <branch>] [--install auto|true|false] [--supabase auto|true|false] [--supabase-config <path>] [--db auto|true|false]
wrt root status
wrt new <name> [--from <ref>] [--branch <branch>] [--install auto|true|false] [--supabase auto|shared|isolated|none] [--supabase-config <path>] [--db auto|true|false] [--cd]
wrt add <name> [--from <ref>] [--branch <branch>] [--install auto|true|false] [--supabase auto|shared|isolated|none] [--supabase-config <path>] [--db auto|true|false] [--cd]
wrt db [<name>] [--worktree <name>] reset [--yes] [--print]
wrt db [<name>] [--worktree <name>] seed|migrate [--print]
wrt ls
wrt path <name>
wrt env [<name>]
wrt doctor [<name>]
wrt setup <name>
wrt runtime [<name>] [--worktree <name>] start|stop|status
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

wrt setup perf-agent-01

wrt runtime perf-agent-01 start
wrt runtime perf-agent-01 status
wrt runtime perf-agent-01 stop

# persist a nested Supabase project path for main and future worktrees
wrt clone git@github.com:org/monorepo.git --supabase-config apps/api/supabase/config.toml

# remove a worktree; interactive runs ask whether to delete its branch
wrt rm x --force
wrt remove x --force
# skip the prompt and delete the local branch plus its configured upstream
wrt rm x --force --delete-branch

# prune stale state entries after manual deletions
wrt prune

# dry-run branch cleanup; add --apply to delete candidates
wrt housekeeping
```

Interactive `wrt rm`/`wrt remove` runs default to keeping the local and upstream
branches. Pass `--delete-branch` to confirm branch cleanup non-interactively.

---

## How It Works

- **Worktree paths**
  - `wrt new <name>` creates the slug as a sibling of `main` (example: `<root>/a-gpt-fix-login-timeout`)
  - `wrt clone <source>` derives `<root>` from the repo URL, then creates `<root>/.git` and `<root>/main`
  - `wrt root init <source> --root <dir>` creates `<dir>/.git` as the bare common repo and `<dir>/main` as block `0`
  - branch names keep slashes, but spaces are normalized to `-`
- **State**
  - tracked in `<git-common-dir>/.wrt/state.json` (usually `.git/.wrt/state.json`)
  - block numbers label candidate offsets; the allocator accepts a candidate only when its complete host-port set is valid and unclaimed
  - concrete project and isolated Supabase ports are reserved together under a state lock and persist for the life of the worktree
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

On the legacy setup path, clone starts one unpatched stack from `main` when it detects a Supabase config. `wrt new` and `wrt add` ask whether to create an isolated stack. Answering no reuses main. In non-interactive use, `auto` reuses main when available and otherwise disables Supabase. A declared `commands.setup` command replaces this automatic lifecycle.

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
- `wrt env`, `wrt doctor`, and `wrt runtime` with no `<name>` only work when you run them from inside a tracked worktree
- Clone fails its setup phase if a detected Supabase project cannot start or report status. The managed root and failed state remain available for recovery.
- Compose preflight blocks a declared `commands.setup` command and `wrt runtime start` when published host ports stay fixed or `container_name` stays fixed across worktrees.
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

Codex discovery runs with a read-only sandbox, no persistent session, and a two-minute timeout. Repository files are treated as untrusted input. `wrt init --print` only prints the validated version 2 config. If discovery returns commands, an interactive run lists each argv and cwd before asking for confirmation. Non-interactive runs must pass `--accept-commands`. `--force` only controls overwriting an existing config.

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

The Docker acceptance test creates two temporary managed worktrees. It starts two Compose stacks and two source-mode HTTP processes, verifies Postgres and Redis isolation, then removes only its own containers, networks, volumes, and processes. Run it explicitly:

```bash
cargo test --test compose_e2e -- --ignored --nocapture
```

---

<div align="center">

Built for people who keep 6 worktrees open and still want predictable ports.

</div>
