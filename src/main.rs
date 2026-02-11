use anyhow::{Context, Result};
use chrono::SecondsFormat;
use clap::{Parser, Subcommand};
use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::{fs, io::Write};

mod codex;
mod db;
mod gitx;
mod pm;
mod state;
mod supabase;
mod ui;
mod worktree;

const USAGE_TEXT: &str = r#"wrt: git worktree helper geared for parallel (agentic) workflows

Usage:
  wrt init [--force] [--print] [--model <codex-model>]
  wrt new <name> [--from <ref>] [--branch <branch>] [--install auto|true|false] [--supabase auto|true|false] [--db auto|true|false] [--cd]
  wrt db [<name>] reset|seed|migrate [--print]
  wrt ls
  wrt path <name>
  wrt env [<name>]
  wrt rm <name> [--force] [--delete-branch]
  wrt prune
  wrt run <name> -- <command> [args...]

Conventions:
  - Worktrees live under: <repo>/.worktrees/<name>
  - Each worktree gets a reserved "port block" (offset = block*100); block 0 is kept for the main workdir.
  - If a Supabase config exists (supabase/config.toml), wrt can patch it to avoid port/container collisions.
  - If DB reset/seed commands are discovered (via .wrt.json), wrt can optionally run them after setup.
"#;

#[derive(Parser, Debug)]
#[command(name = "wrt")]
#[command(disable_version_flag = true)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print usage
    Help,

    /// Generate repo-local config via Codex (writes .wrt.json)
    Init {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        print: bool,
        #[arg(long)]
        model: Option<String>,
    },

    /// Create a new worktree (+branch), optionally install deps and start supabase
    New {
        name: String,
        #[arg(long, default_value = "HEAD")]
        from: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long, default_value = "auto")]
        install: String,
        #[arg(long, default_value = "auto")]
        supabase: String,
        #[arg(long, default_value = "auto")]
        db: String,
        /// Print a `cd <path>` snippet to stdout after creation (use with `eval "$(wrt new ... --cd)"`)
        #[arg(long)]
        cd: bool,
    },

    /// Run database utilities for a worktree (reset/seed/migrate)
    Db {
        /// Worktree name (optional if run from inside a worktree directory)
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Explicit worktree name (useful if the name conflicts with a subcommand like "reset")
        #[arg(long, value_name = "NAME")]
        worktree: Option<String>,
        #[command(subcommand)]
        action: DbAction,
    },

    /// List tracked worktrees
    Ls,
    /// Alias for ls
    List,

    /// Print worktree path
    Path { name: String },

    /// Print exports for the current worktree (or pass a name)
    Env { name: Option<String> },

    /// Remove a worktree
    Rm {
        name: String,
        #[arg(long)]
        force: bool,
        #[arg(long = "delete-branch")]
        delete_branch: bool,
    },
    /// Alias for rm
    Remove {
        name: String,
        #[arg(long)]
        force: bool,
        #[arg(long = "delete-branch")]
        delete_branch: bool,
    },

    /// Prune git worktrees and state
    Prune,
    /// Run a command inside a worktree with WRT_* env vars set
    ///
    /// Must be invoked as: wrt run <name> -- <command> [args...]
    #[command(trailing_var_arg = true)]
    Run {
        name: String,
        #[arg(required = true, value_name = "COMMAND", num_args = 1.., allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum DbAction {
    /// Reset the local database (destructive)
    Reset {
        /// Skip interactive prompts (required in non-interactive contexts)
        #[arg(long)]
        yes: bool,
        /// Print the command that would be run and exit
        #[arg(long)]
        print: bool,
    },
    /// Seed the local database
    Seed {
        /// Print the command that would be run and exit
        #[arg(long)]
        print: bool,
    },
    /// Run migrations
    Migrate {
        /// Print the command that would be run and exit
        #[arg(long)]
        print: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("[wrt] ERROR: {e}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<i32> {
    let log = ui::Logger;
    let raw_args: Vec<String> = env::args().collect();

    let cli = Cli::parse();
    let Some(cmd) = cli.cmd else {
        eprintln!("{USAGE_TEXT}");
        return Ok(2);
    };

    if matches!(&cmd, Cmd::Help) {
        print!("{USAGE_TEXT}");
        return Ok(0);
    }

    let cwd = env::current_dir()?;
    let repo = match gitx::detect_repo(&cwd) {
        Ok(r) => r,
        Err(e) => {
            log.errorf(&format!("not a git repo (or git not available): {e}"));
            return Ok(2);
        }
    };

    let _ = gitx::ensure_info_exclude(&repo.common_dir, &[".worktrees/", ".wrt.env", ".wrt.json"]);

    let mut st = match state::State::load(&repo.common_dir) {
        Ok(s) => s,
        Err(e) => {
            log.errorf(&format!("state load failed: {e}"));
            return Ok(1);
        }
    };

    match cmd {
        Cmd::Help => {
            print!("{USAGE_TEXT}");
            Ok(0)
        }

        Cmd::Init {
            force,
            print,
            model,
        } => cmd_init(&log, &repo.root, force, print, model),
        Cmd::New {
            name,
            from,
            branch,
            install,
            supabase,
            db,
            cd,
        } => {
            let opts = NewOpts {
                name: &name,
                from_ref: &from,
                branch: branch.as_deref(),
                install_mode: &install,
                sb_mode: &supabase,
                db_mode: &db,
                emit_cd: cd,
            };
            cmd_new(&log, &repo, &mut st, opts)
        }
        Cmd::Db {
            name,
            worktree,
            action,
        } => cmd_db(
            &log,
            &repo,
            &st,
            name.as_deref(),
            worktree.as_deref(),
            action,
        ),
        Cmd::Ls | Cmd::List => cmd_ls(&st),
        Cmd::Path { name } => cmd_path(&log, &st, &name),
        Cmd::Env { name } => cmd_env(&log, &st, name.as_deref()),
        Cmd::Rm {
            name,
            force,
            delete_branch,
        }
        | Cmd::Remove {
            name,
            force,
            delete_branch,
        } => cmd_rm(&log, &repo, &mut st, &name, force, delete_branch),
        Cmd::Prune => cmd_prune(&log, &repo, &mut st),
        Cmd::Run { name, command } => {
            if !raw_run_has_sep(&raw_args) {
                log.errorf("usage: wrt run <name> -- <command> [args...]");
                return Ok(2);
            }
            cmd_run(&log, &st, &name, &command)
        }
    }
}

fn raw_run_has_sep(raw_args: &[String]) -> bool {
    // Expect: wrt run <name> -- <cmd> ...
    if raw_args.len() < 4 {
        return false;
    }
    if raw_args.get(1).map(|s| s.as_str()) != Some("run") {
        return true;
    }
    match raw_args.iter().position(|s| s == "--") {
        Some(i) => i == 3,
        None => false,
    }
}

fn cmd_init(
    log: &ui::Logger,
    repo_root: &Path,
    force: bool,
    print_only: bool,
    model: Option<String>,
) -> Result<i32> {
    let out_path = repo_root.join(".wrt.json");
    if !print_only && !force && out_path.exists() {
        log.errorf(&format!(
            "{} already exists (use --force to overwrite)",
            out_path.display()
        ));
        return Ok(2);
    }

    log.infof("running codex discovery (writes .wrt.json config)");
    let (raw, _) = match codex::discover(codex::DiscoverOpts {
        repo_root: repo_root.to_path_buf(),
        model,
    }) {
        Ok(v) => v,
        Err(e) => {
            log.errorf(&format!("{e}"));
            log.errorf("hint: install/auth codex CLI, or set WRT_CODEX_MOCK_OUTPUT=/path/to/out.json for offline testing");
            return Ok(1);
        }
    };

    let v: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            log.errorf(&format!("codex output is not valid JSON: {e}"));
            return Ok(1);
        }
    };

    let mut pretty = serde_json::to_string_pretty(&v)?.into_bytes();
    pretty.push(b'\n');

    if print_only {
        std::io::stdout().write_all(&pretty)?;
        return Ok(0);
    }

    fs::write(&out_path, &pretty).with_context(|| format!("write {}", out_path.display()))?;
    log.infof(&format!("wrote {}", out_path.display()));
    Ok(0)
}

struct NewOpts<'a> {
    name: &'a str,
    from_ref: &'a str,
    branch: Option<&'a str>,
    install_mode: &'a str,
    sb_mode: &'a str,
    db_mode: &'a str,
    emit_cd: bool,
}

fn cmd_new(
    log: &ui::Logger,
    repo: &gitx::Repo,
    st: &mut state::State,
    opts: NewOpts<'_>,
) -> Result<i32> {
    let wt_name = worktree::slug(opts.name);
    let wt_path = repo.root.join(".worktrees").join(&wt_name);

    let mut br = opts.branch.unwrap_or("").trim().to_string();
    if br.is_empty() {
        // If user passes "a/foo/bar", keep it as-is for the branch and slug it for the dir.
        br = opts.name.to_string();
    }
    br = worktree::normalize_branch(&br);

    if st.allocations.contains_key(&wt_name) {
        log.errorf(&format!(
            "worktree \"{wt_name}\" already exists in state; use `wrt ls`"
        ));
        return Ok(2);
    }

    let block = match st.allocate_block() {
        Ok(b) => b,
        Err(e) => {
            log.errorf(&format!("allocate port block: {e}"));
            return Ok(1);
        }
    };
    let offset = block * 100;

    log.infof(&format!(
        "creating worktree: {wt_name} ({br}) at {}",
        wt_path.display()
    ));

    worktree::ensure_dir(wt_path.parent().unwrap())?;

    if let Err(e) = worktree::add(&repo.root, &wt_path, &br, opts.from_ref) {
        log.errorf(&format!("git worktree add failed: {e}"));
        return Ok(1);
    }

    let created_at = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let alloc = state::Allocation {
        name: wt_name.clone(),
        branch: br.clone(),
        path: wt_path.to_string_lossy().to_string(),
        block,
        offset,
        created_at,
    };

    st.allocations.insert(wt_name.clone(), alloc.clone());
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    if let Err(e) = worktree::write_env_file(&wt_path, &alloc) {
        log.errorf(&format!("write env file: {e}"));
        return Ok(1);
    }

    match worktree::copy_repo_env(&repo.root, &wt_path) {
        Ok(true) => log.infof("copied .env from repo root"),
        Ok(false) => {}
        Err(e) => log.infof(&format!("copy .env failed: {e}")),
    }

    let sb = opts.sb_mode.trim().to_lowercase();
    let install = opts.install_mode.trim().to_lowercase();
    let db_mode = opts.db_mode.trim().to_lowercase();

    if sb == "true" || (sb == "auto" && supabase::has_config(&wt_path)) {
        log.infof("supabase detected: patching config for isolation (project_id + ports)");
        if let Err(e) = supabase::patch_config(&wt_path, &wt_name, offset) {
            log.errorf(&format!("supabase patch failed: {e}"));
            return Ok(1);
        }
        let _ = run_cmd(
            &wt_path,
            "git",
            &["update-index", "--skip-worktree", "supabase/config.toml"],
        );
    }

    if install == "true" || (install == "auto" && pm::has_project(&wt_path)) {
        if let Some((cmd, args)) = pm::detect_install_command(&wt_path) {
            log.infof(&format!("install: {cmd} {}", args.join(" ")));
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            if let Err(e) = run_cmd(&wt_path, &cmd, &arg_refs) {
                log.errorf(&format!("install failed: {e}"));
                return Ok(1);
            }
        } else {
            log.infof("no package manager detected; skipping install");
        }
    }

    if sb == "true" || (sb == "auto" && supabase::has_config(&wt_path)) {
        if which("supabase").is_none() {
            log.infof("supabase CLI not found; skipping `supabase start` (config patched)");
        } else {
            log.infof("supabase start (isolated ports, project_id suffix)");
            if let Err(e) = run_cmd(&wt_path, "supabase", &["start"]) {
                log.errorf(&format!("supabase start failed: {e}"));
                return Ok(1);
            }
        }
    }

    if db_mode != "false" {
        if let Err(e) = maybe_run_db_setup(log, repo, &alloc, &wt_path, &db_mode) {
            log.errorf(&format!("db setup failed: {e}"));
            return Ok(1);
        }
    }

    if opts.emit_cd {
        println!("cd {}", sh_quote(&wt_path.to_string_lossy()));
    }

    Ok(0)
}

fn cmd_db(
    log: &ui::Logger,
    repo: &gitx::Repo,
    st: &state::State,
    name: Option<&str>,
    worktree: Option<&str>,
    action: DbAction,
) -> Result<i32> {
    let mut resolved = worktree
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));

    if resolved.is_none() {
        resolved = infer_worktree_from_cwd(st);
    }

    let Some(resolved) = resolved else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&resolved);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    let wt_path = PathBuf::from(&a.path);
    let cfg_path = repo.root.join(".wrt.json");

    let mut kind_hint: Option<String> = None;
    let mut cmd: Option<Vec<String>> = None;
    let (op, yes, print) = match action {
        DbAction::Reset { yes, print } => ("reset", yes, print),
        DbAction::Seed { print } => ("seed", false, print),
        DbAction::Migrate { print } => ("migrate", false, print),
    };

    if cfg_path.exists() {
        if let Ok(s) = fs::read_to_string(&cfg_path) {
            if let Ok(d) = serde_json::from_str::<codex::Discovery>(&s) {
                if d.database.detected {
                    kind_hint = d.database.kind.clone();
                }
                cmd = match op {
                    "reset" => d.database.reset_command.clone(),
                    "seed" => d.database.seed_command.clone(),
                    "migrate" => d.database.migrate_command.clone(),
                    _ => None,
                };
            } else {
                log.infof("could not parse .wrt.json; skipping DB setup from config");
            }
        }
    }

    // Minimal fallback for Supabase reset if no .wrt.json exists yet.
    if cmd.is_none() && op == "reset" && db::has_supabase_seed_or_migrations(&wt_path) {
        kind_hint = kind_hint.or(Some("supabase".into()));
        cmd = Some(vec!["supabase".into(), "db".into(), "reset".into()]);
    }

    let Some(argv) = cmd else {
        let label = kind_hint.as_deref().unwrap_or("database");
        log.errorf(&format!(
            "{label}: no {op} command known; run `wrt init` to generate .wrt.json"
        ));
        return Ok(2);
    };
    if argv.is_empty() {
        return Ok(0);
    }

    let label = kind_hint.as_deref().unwrap_or("database");
    let cmd_str = argv.join(" ");

    if print {
        println!("{cmd_str}");
        return Ok(0);
    }

    if op == "reset" {
        if yes {
            // ok
        } else if !std::io::stdin().is_terminal() {
            log.errorf(&format!(
                "{label}: refusing to run reset non-interactively; pass `--yes` to confirm"
            ));
            return Ok(2);
        } else if !confirm(&format!(
            "{label}: run DB reset now? This may delete local data. [{cmd_str}] (y/N): "
        ))? {
            log.infof(&format!("{label}: skipping reset"));
            return Ok(0);
        }
    }

    log.infof(&format!("{label}: running: {cmd_str}"));
    if let Err(e) = run_argv_with_wrt_env(&wt_path, a, &argv) {
        log.errorf(&format!("{label}: command failed: {e}"));
        return Ok(1);
    }
    Ok(0)
}

fn infer_worktree_from_cwd(st: &state::State) -> Option<String> {
    let wd = env::current_dir().ok()?;
    for a in st.allocations.values() {
        let ap = PathBuf::from(&a.path);
        if wd.strip_prefix(&ap).is_ok() {
            return Some(a.name.clone());
        }
    }
    None
}

fn maybe_run_db_setup(
    log: &ui::Logger,
    repo: &gitx::Repo,
    alloc: &state::Allocation,
    wt_path: &Path,
    db_mode: &str,
) -> Result<()> {
    let mut kind_hint: Option<String> = None;
    let mut reset_cmd: Option<Vec<String>> = None;

    // Prefer explicit repo config (wrt init).
    let cfg_path = repo.root.join(".wrt.json");
    if cfg_path.exists() {
        if let Ok(s) = fs::read_to_string(&cfg_path) {
            if let Ok(d) = serde_json::from_str::<codex::Discovery>(&s) {
                if d.database.detected {
                    kind_hint = d.database.kind.clone();
                }
                // For `wrt new --db ...`, only ever run a reset command. Seed/migrate are explicit
                // operations via `wrt db ...`.
                reset_cmd = d.database.reset_command.clone();
            } else {
                log.infof("could not parse .wrt.json; skipping DB setup from config");
            }
        }
    }

    // Fallback heuristic for common Supabase repos without .wrt.json.
    if reset_cmd.is_none() && db::has_supabase_seed_or_migrations(wt_path) {
        kind_hint = kind_hint.or(Some("supabase".into()));
        reset_cmd = Some(vec!["supabase".into(), "db".into(), "reset".into()]);
    }

    // If we still have no actionable command, keep it non-fatal.
    let Some(argv) = reset_cmd else {
        let mut hints: Vec<&str> = Vec::new();
        if db::has_prisma_schema(wt_path) {
            hints.push("prisma");
        }
        if db::has_sqlx_markers(wt_path) {
            hints.push("sqlx");
        }
        if !hints.is_empty() {
            log.infof(&format!(
                "db tooling detected ({}) but no reset command known; run `wrt init` to generate .wrt.json or use `wrt db <name> seed|migrate`",
                hints.join(", ")
            ));
        }
        return Ok(());
    };

    if argv.is_empty() {
        return Ok(());
    }

    let label = kind_hint.as_deref().unwrap_or("database");
    let cmd_str = argv.join(" ");

    match db_mode {
        "true" => {
            log.infof(&format!("{label}: running db setup: {cmd_str}"));
            run_argv_with_wrt_env(wt_path, alloc, &argv)?;
        }
        "auto" => {
            if !std::io::stdin().is_terminal() {
                log.infof(&format!(
                    "{label}: db setup available ({cmd_str}) but skipping in non-interactive mode; rerun with `--db true` to run"
                ));
                return Ok(());
            }

            if !confirm(&format!(
                "{label}: run DB reset/seed now? This may delete local data. [{cmd_str}] (y/N): "
            ))? {
                log.infof(&format!("{label}: skipping db setup"));
                return Ok(());
            }

            log.infof(&format!("{label}: running db setup: {cmd_str}"));
            run_argv_with_wrt_env(wt_path, alloc, &argv)?;
        }
        "false" => {}
        _ => {
            log.infof("invalid --db value (expected auto|true|false); skipping db setup");
        }
    }

    Ok(())
}

fn run_argv_with_wrt_env(dir: &Path, a: &state::Allocation, argv: &[String]) -> Result<()> {
    let cmd = &argv[0];
    let cmd_args = &argv[1..];

    let mut envs: Vec<(String, String)> = env::vars().collect();
    envs.push(("WRT_NAME".into(), a.name.clone()));
    envs.push(("WRT_BRANCH".into(), a.branch.clone()));
    envs.push(("WRT_PORT_BLOCK".into(), a.block.to_string()));
    envs.push(("WRT_PORT_OFFSET".into(), a.offset.to_string()));

    let mut c = Command::new(cmd);
    c.args(cmd_args)
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit());

    c.env_clear();
    for (k, v) in envs {
        c.env(k, v);
    }

    let status = c.status().with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("command failed"));
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::{self, Write};

    eprint!("{prompt}");
    io::stderr().flush().ok();

    let mut s = String::new();
    io::stdin().read_line(&mut s).context("read user input")?;
    let ans = s.trim().to_lowercase();
    Ok(ans == "y" || ans == "yes")
}

fn cmd_ls(st: &state::State) -> Result<i32> {
    if st.allocations.is_empty() {
        println!("(no worktrees tracked by wrt)");
        return Ok(0);
    }

    for a in st.sorted_allocations() {
        let dirty = match worktree::is_dirty(Path::new(&a.path)) {
            Ok(true) => "dirty",
            Ok(false) => "clean",
            Err(_) => "?",
        };
        println!(
            "{:<28}  block={:<3}  offset={:<4}  {:<5}  {}  ({})",
            a.name, a.block, a.offset, dirty, a.branch, a.path
        );
    }

    Ok(0)
}

fn cmd_path(log: &ui::Logger, st: &state::State, name: &str) -> Result<i32> {
    let key = worktree::slug(name);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };
    println!("{}", a.path);
    Ok(0)
}

fn cmd_env(log: &ui::Logger, st: &state::State, name: Option<&str>) -> Result<i32> {
    let mut name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    if name.is_none() {
        name = infer_worktree_from_cwd(st);
    }

    let Some(name) = name else {
        log.errorf("missing <name> (or run inside a worktree)");
        return Ok(2);
    };

    let key = worktree::slug(&name);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    println!("export WRT_NAME={}", sh_quote(&a.name));
    println!("export WRT_BRANCH={}", sh_quote(&a.branch));
    println!("export WRT_PORT_BLOCK={}", a.block);
    println!("export WRT_PORT_OFFSET={}", a.offset);
    Ok(0)
}

fn cmd_rm(
    log: &ui::Logger,
    repo: &gitx::Repo,
    st: &mut state::State,
    name: &str,
    force: bool,
    delete_branch: bool,
) -> Result<i32> {
    let key = worktree::slug(name);
    let Some(a) = st.allocations.get(&key).cloned() else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    log.infof(&format!("removing worktree: {} ({})", a.name, a.path));

    let wt_path = Path::new(&a.path);
    if wt_path.exists() && supabase::has_config(wt_path) {
        if which("supabase").is_some() {
            log.infof("stopping supabase containers");
            if let Err(e) = run_cmd(wt_path, "supabase", &["stop"]) {
                log.errorf(&format!("supabase stop failed: {e}"));
                if !force {
                    return Ok(1);
                }
                log.infof("continuing anyway (--force)");
            }
        }
    }

    if let Err(e) = worktree::remove(&repo.root, wt_path, force) {
        log.errorf(&format!("git worktree remove failed: {e}"));
        return Ok(1);
    }

    if delete_branch {
        log.infof(&format!("deleting branch: {}", a.branch));
        if let Err(e) = run_cmd(&repo.root, "git", &["branch", "-D", &a.branch]) {
            log.errorf(&format!("branch delete failed: {e}"));
            return Ok(1);
        }
    }

    st.allocations.remove(&key);
    if let Err(e) = st.save(&repo.common_dir) {
        log.errorf(&format!("state save failed: {e}"));
        return Ok(1);
    }

    Ok(0)
}

fn cmd_prune(log: &ui::Logger, repo: &gitx::Repo, st: &mut state::State) -> Result<i32> {
    log.infof("git worktree prune");
    if let Err(e) = run_cmd(&repo.root, "git", &["worktree", "prune"]) {
        log.errorf(&format!("prune failed: {e}"));
        return Ok(1);
    }

    let mut removed = 0;
    let keys: Vec<String> = st.allocations.keys().cloned().collect();
    for k in keys {
        let missing = st
            .allocations
            .get(&k)
            .map(|a| !Path::new(&a.path).exists())
            .unwrap_or(false);
        if missing {
            st.allocations.remove(&k);
            removed += 1;
        }
    }

    if removed > 0 {
        log.infof(&format!("state: removed {removed} missing worktrees"));
        if let Err(e) = st.save(&repo.common_dir) {
            log.errorf(&format!("state save failed: {e}"));
            return Ok(1);
        }
    }

    Ok(0)
}

fn cmd_run(log: &ui::Logger, st: &state::State, name: &str, command: &[String]) -> Result<i32> {
    if command.is_empty() {
        log.errorf("usage: wrt run <name> -- <command> [args...]");
        return Ok(2);
    }

    let key = worktree::slug(name);
    let Some(a) = st.allocations.get(&key) else {
        log.errorf(&format!("unknown worktree: \"{key}\""));
        return Ok(2);
    };

    let cmd = &command[0];
    let cmd_args = &command[1..];

    let mut envs: Vec<(String, String)> = env::vars().collect();
    envs.push(("WRT_NAME".into(), a.name.clone()));
    envs.push(("WRT_BRANCH".into(), a.branch.clone()));
    envs.push(("WRT_PORT_BLOCK".into(), a.block.to_string()));
    envs.push(("WRT_PORT_OFFSET".into(), a.offset.to_string()));

    log.infof(&format!(
        "run: {cmd} {} (in {})",
        cmd_args.join(" "),
        a.path
    ));

    let mut c = Command::new(cmd);
    c.args(cmd_args)
        .current_dir(&a.path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit());

    c.env_clear();
    for (k, v) in envs {
        c.env(k, v);
    }

    let status = match c.status() {
        Ok(s) => s,
        Err(e) => {
            log.errorf(&format!("run failed: {e}"));
            return Ok(1);
        }
    };

    Ok(status.code().unwrap_or(1))
}

fn run_cmd(dir: &Path, cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .with_context(|| format!("run {cmd}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("command failed"));
    }
    Ok(())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for p in env::split_paths(&path) {
        let cand = p.join(bin);
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

fn sh_quote(s: &str) -> String {
    // Safe for POSIX shells: ' -> '\''
    let mut out = String::from("'");
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}
