use anyhow::Result;
use clap::Parser;
use std::env;
use std::process::ExitCode;

mod cli;
mod cmd;
mod codex;
mod completions;
mod compose;
mod db;
mod envx;
mod gitx;
mod pm;
mod project;
mod state;
mod supabase;
mod ui;
mod util;
mod worktree;

use cli::{Cli, Cmd, RootAction, USAGE_TEXT};
use cmd::{
    CloneOpts, NewOpts, RootInitOpts, cmd_clone, cmd_db, cmd_doctor, cmd_env, cmd_housekeeping,
    cmd_init, cmd_ls, cmd_new, cmd_path, cmd_prune, cmd_rm, cmd_root_init, cmd_root_status,
    cmd_run, cmd_runtime, cmd_setup, raw_run_has_sep,
};

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

    let cmd = match cmd {
        Cmd::Help => {
            print!("{USAGE_TEXT}");
            return Ok(0);
        }
        Cmd::Completions { shell } => {
            if shell.trim().eq_ignore_ascii_case("zsh") {
                print!("{}", completions::zsh_script());
                return Ok(0);
            }
            log.errorf("unsupported shell (only zsh is available)");
            return Ok(2);
        }
        Cmd::Clone {
            source,
            root,
            main,
            install,
            supabase,
            supabase_config,
            db,
        } => {
            let opts = CloneOpts {
                source: &source,
                root: root.as_deref(),
                main: main.as_deref(),
                install_mode: install.as_str(),
                sb_mode: supabase,
                supabase_config: supabase_config.as_deref(),
                db_mode: db.as_str(),
            };
            return cmd_clone(&log, opts);
        }
        Cmd::Root {
            action:
                RootAction::Init {
                    source,
                    root,
                    main,
                    install,
                    supabase,
                    supabase_config,
                    db,
                },
        } => {
            let opts = RootInitOpts {
                source: &source,
                root: &root,
                main: main.as_deref(),
                install_mode: install.as_str(),
                sb_mode: supabase,
                supabase_config: supabase_config.as_deref(),
                db_mode: db.as_str(),
            };
            return cmd_root_init(&log, opts);
        }
        other => other,
    };

    let cwd = env::current_dir()?;
    let repo = match gitx::detect_repo(&cwd) {
        Ok(r) => r,
        Err(e) => {
            log.errorf(&format!(
                "not a wrt managed root (or git not available): {e}"
            ));
            return Ok(2);
        }
    };

    let _ = gitx::ensure_info_exclude(
        &repo.common_dir,
        &[".env", ".env.local", ".wrt.env", ".wrt.json"],
    );

    let store = state::StateStore::new(&repo);
    let mut st = match store.read() {
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
            accept_commands,
            model,
        } => cmd_init(
            &log,
            &repo.root,
            &repo.config_root,
            force,
            print,
            accept_commands,
            model,
        ),

        Cmd::Clone { .. } => Ok(0),

        Cmd::Root {
            action: RootAction::Status,
        } => cmd_root_status(&log, &repo, &st),

        Cmd::Root {
            action: RootAction::Init { .. },
        } => Ok(0),

        Cmd::New {
            name,
            from,
            branch,
            install,
            supabase,
            supabase_config,
            db,
            cd,
        } => {
            let opts = NewOpts {
                name: &name,
                from_ref: &from,
                branch: branch.as_deref(),
                install_mode: install.as_str(),
                sb_mode: supabase,
                supabase_config: supabase_config.as_deref(),
                db_mode: db.as_str(),
                emit_cd: cd,
            };
            cmd_new(&log, &repo, &store, &mut st, opts)
        }

        Cmd::Db {
            name,
            worktree,
            action,
        } => cmd_db(
            &log,
            &repo,
            &store,
            &st,
            name.as_deref(),
            worktree.as_deref(),
            action,
        ),

        Cmd::Ls => cmd_ls(&st),

        Cmd::Path { name } => cmd_path(&log, &st, &name),

        Cmd::Env { name } => cmd_env(&log, &repo, &store, &st, name.as_deref()),

        Cmd::Doctor { name } => cmd_doctor(&log, &repo, &st, name.as_deref()),

        Cmd::Setup { name } => cmd_setup(&log, &repo, &store, &mut st, &name),

        Cmd::Runtime {
            name,
            worktree,
            action,
        } => cmd_runtime(
            &log,
            &repo,
            &store,
            &st,
            name.as_deref(),
            worktree.as_deref(),
            action,
        ),

        Cmd::Rm {
            name,
            force,
            delete_branch,
        } => cmd_rm(&log, &repo, &store, &mut st, &name, force, delete_branch),

        Cmd::Prune => cmd_prune(&log, &repo, &store, &mut st),

        Cmd::Housekeeping { apply } => cmd_housekeeping(&log, &repo, apply),

        Cmd::Run { name, command } => {
            if !raw_run_has_sep(&raw_args) {
                log.errorf("usage: wrt run <name> -- <command> [args...]");
                return Ok(2);
            }
            cmd_run(&log, &repo, &store, &st, &name, &command)
        }
        Cmd::Completions { .. } => Ok(0),
    }
}
