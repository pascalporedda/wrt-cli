use clap::{Parser, Subcommand};

pub const USAGE_TEXT: &str = r#"wrt: git worktree helper geared for parallel (agentic) workflows

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
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
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
pub enum DbAction {
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
