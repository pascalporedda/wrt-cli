use clap::{Parser, Subcommand, ValueEnum};

pub const USAGE_TEXT: &str = r#"wrt: git worktree helper geared for parallel (agentic) workflows

Usage:
  wrt init [--force] [--print] [--model <codex-model>]  (default model: gpt-5.6-sol, reasoning medium)
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

Conventions:
  - Managed roots live under: <root>/.git + <root>/main + <root>/<feature>
  - Feature worktrees live as siblings of main: <root>/<name>
  - Each worktree gets a reserved "port block" (offset = block*100); block 0 is kept for the main workdir.
  - Supabase repos get one shared main instance; feature worktrees can reuse it or request isolation.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RootSupabaseMode {
    Auto,
    True,
    False,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FeatureSupabaseMode {
    Auto,
    Shared,
    #[value(alias = "true")]
    Isolated,
    #[value(alias = "false")]
    None,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Print usage
    Help,

    /// Generate shared managed-root config via Codex (writes .wrt.json)
    Init {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        print: bool,
        #[arg(long)]
        model: Option<String>,
    },

    /// Clone a git repo into a managed root and run setup
    Clone {
        source: String,
        #[arg(long)]
        root: Option<String>,
        #[arg(long)]
        main: Option<String>,
        #[arg(long, default_value = "auto")]
        install: String,
        #[arg(long, value_enum, default_value_t = RootSupabaseMode::Auto)]
        supabase: RootSupabaseMode,
        #[arg(long = "supabase-config", value_name = "PATH")]
        supabase_config: Option<String>,
        #[arg(long, default_value = "auto")]
        db: String,
    },

    /// Manage bare-root wrt environments
    Root {
        #[command(subcommand)]
        action: RootAction,
    },

    /// Create a new worktree (+branch), optionally install deps and set up supabase
    #[command(visible_alias = "add")]
    New {
        name: String,
        #[arg(long, default_value = "HEAD")]
        from: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long, default_value = "auto")]
        install: String,
        #[arg(long, value_enum, default_value_t = FeatureSupabaseMode::Auto)]
        supabase: FeatureSupabaseMode,
        #[arg(long = "supabase-config", value_name = "PATH")]
        supabase_config: Option<String>,
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

    /// Print worktree path
    Path { name: String },

    /// Print exports for the current worktree (or pass a name)
    Env { name: Option<String> },

    /// Remove a worktree
    #[command(visible_alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        force: bool,
        #[arg(long = "delete-branch")]
        delete_branch: bool,
    },

    /// Prune git worktrees and state
    Prune,

    /// Clean old merged branches not attached to worktrees
    Housekeeping {
        /// Delete candidates instead of printing a dry-run
        #[arg(long)]
        apply: bool,
    },

    /// Run a command inside a worktree with WRT_* env vars set
    ///
    /// Must be invoked as: wrt run <name> -- <command> [args...]
    #[command(trailing_var_arg = true)]
    Run {
        name: String,
        #[arg(required = true, value_name = "COMMAND", num_args = 1.., allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Generate shell completions (currently zsh only)
    Completions { shell: String },
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

#[derive(Subcommand, Debug, Clone)]
pub enum RootAction {
    /// Create a managed root with a bare common repo and main worktree
    Init {
        source: String,
        #[arg(long)]
        root: String,
        #[arg(long)]
        main: Option<String>,
        #[arg(long, default_value = "auto")]
        install: String,
        #[arg(long, value_enum, default_value_t = RootSupabaseMode::Auto)]
        supabase: RootSupabaseMode,
        #[arg(long = "supabase-config", value_name = "PATH")]
        supabase_config: Option<String>,
        #[arg(long, default_value = "auto")]
        db: String,
    },
    /// Print managed-root status
    Status,
}
