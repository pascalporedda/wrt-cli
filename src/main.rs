use anyhow::Result;
use clap::Parser;
use std::env;
use std::process::ExitCode;

mod cli;
mod cmd;
mod codex;
mod db;
mod gitx;
mod pm;
mod state;
mod supabase;
mod ui;
mod util;
mod worktree;

use cli::{Cli, Cmd, USAGE_TEXT};
use cmd::{
    cmd_db, cmd_env, cmd_init, cmd_ls, cmd_new, cmd_path, cmd_prune, cmd_rm, cmd_run,
    raw_run_has_sep, NewOpts,
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
