---
name: wrt-worktrees
description: Manage Git worktrees with the wrt CLI, including discovering managed roots, creating isolated task worktrees, locating or entering them, running commands with their environment, listing state, and removing or pruning them safely. Use automatically whenever a task involves worktrees, parallel agent branches, isolated checkouts, or any create/list/path/run/remove/prune worktree operation in a repository where wrt may be available; prefer this workflow over raw `git worktree` commands for wrt-managed roots.
---

# Manage worktrees with `wrt`

Use `wrt` as the lifecycle owner in a wrt-managed root. It tracks worktrees, assigns port blocks, writes environment files, and coordinates optional Supabase isolation.

## Start by inspecting

Run:

```bash
wrt root status
wrt ls
```

If `wrt root status` says the repository is not a managed root, do not run `wrt clone` or `wrt root init` unless the user explicitly asks to create or migrate a managed root. Report the limitation and follow the repository's instructions.

## Create a task worktree

Create from the desired ref, then resolve its actual path:

```bash
wrt new <name> --from <ref>
wrt path <name>
```

- Use a descriptive slash-separated name such as `a/codex/fix-login-timeout`.
- Omit `--from` only when the current `HEAD` is definitely the intended base.
- Add `--branch <branch>` only when the branch must differ from the name.
- For deterministic automation, explicitly choose setup modes when relevant: `--install true|false`, `--supabase shared|isolated|none`, and `--db true|false`.
- Treat `--db true` as destructive setup, especially with `--supabase shared`; use it only when explicitly intended.

The branch may keep slashes while the tracked name and directory are slugged. Pass either the original name or its slug to later `wrt` commands.

## Work inside it

Set subsequent tool calls' working directory to the result of `wrt path <name>`. In a persistent shell, use:

```bash
cd "$(wrt path <name>)"
eval "$(wrt env)"
```

For one command without changing directories, use the required separator:

```bash
wrt run <name> -- <command> [args...]
```

Do all task edits and checks—and commits when requested—in the task worktree, not in `main` or the checkout from which it was created.

## Inspect and clean up

```bash
wrt ls
wrt rm <name>
wrt prune
```

- Inspect `wrt ls` before removing anything; it shows path, branch, dirty state, ports, and Supabase mode.
- When cleanup is requested, use `wrt rm <name>` so wrt can stop owned services and update its state.
- Use `--force` only after checking uncommitted work and with authorization for destructive cleanup.
- Add `--delete-branch` only when branch deletion is explicitly desired.
- Use `wrt prune` only to reconcile state after worktree directories were removed outside wrt.
- Never manually edit `.git/.wrt/state.json`, `.wrt.env`, or wrt's Supabase isolation patches.
