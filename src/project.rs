use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectConfig {
    port_stride: u16,
    ports: Vec<PortSpec>,
    commands: ProjectCommands,
    compose: Option<ComposeSpec>,
    supabase: Option<SupabaseProject>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortSpec {
    key: PortKey,
    base_port: u16,
    outputs: Vec<EnvOutput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvOutput {
    env: EnvName,
    template: PortTemplate,
}

impl EnvOutput {
    pub fn env(&self) -> &str {
        &self.env.0
    }

    pub fn render(&self, port: u16) -> String {
        self.template.0.replace("{port}", &port.to_string())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectCommands {
    setup: Option<CommandSpec>,
    start: Option<CommandSpec>,
    stop: Option<CommandSpec>,
    status: Option<CommandSpec>,
    db_migrate: Option<CommandSpec>,
    db_seed: Option<CommandSpec>,
    db_reset: Option<CommandSpec>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    argv: Vec<String>,
    cwd: Option<RepoRelativePath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposeSpec {
    files: Vec<RepoRelativePath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupabaseProject {
    config_path: RepoRelativePath,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PortKey(String);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EnvName(String);

#[derive(Clone, Debug, Eq, PartialEq)]
struct PortTemplate(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRelativePath(PathBuf);

impl PortSpec {
    pub fn key(&self) -> &PortKey {
        &self.key
    }

    pub fn base_port(&self) -> u16 {
        self.base_port
    }

    pub fn outputs(&self) -> &[EnvOutput] {
        &self.outputs
    }
}

impl PortKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn supabase_claim(path: &[String]) -> Self {
        let encoded = path
            .iter()
            .map(|component| {
                let mut output = String::new();
                for byte in component.bytes() {
                    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                        output.push(char::from(byte));
                    } else {
                        output.push_str(&format!("%{byte:02X}"));
                    }
                }
                output
            })
            .collect::<Vec<_>>()
            .join(".");
        Self(format!("supabase.{encoded}"))
    }

    pub(crate) fn is_valid_state_key(&self) -> bool {
        is_valid_port_key(&self.0)
            || self
                .0
                .strip_prefix("supabase.")
                .is_some_and(|suffix| !suffix.is_empty())
    }
}

impl CommandSpec {
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_ref().map(|path| path.0.as_path())
    }

    pub fn working_dir(&self, worktree_root: &Path) -> Result<PathBuf> {
        let root = fs::canonicalize(worktree_root)
            .with_context(|| format!("resolve worktree root {}", worktree_root.display()))?;
        let target = self
            .cwd
            .as_ref()
            .map_or_else(|| root.clone(), |path| root.join(&path.0));
        let resolved = fs::canonicalize(&target)
            .with_context(|| format!("resolve command cwd {}", target.display()))?;
        if !resolved.starts_with(&root) {
            bail!(
                "command cwd resolves outside the worktree: {}",
                target.display()
            );
        }
        if !resolved.is_dir() {
            bail!("command cwd is not a directory: {}", target.display());
        }
        Ok(resolved)
    }
}

impl ProjectCommands {
    pub fn setup(&self) -> Option<&CommandSpec> {
        self.setup.as_ref()
    }

    pub fn start(&self) -> Option<&CommandSpec> {
        self.start.as_ref()
    }

    pub fn stop(&self) -> Option<&CommandSpec> {
        self.stop.as_ref()
    }

    pub fn status(&self) -> Option<&CommandSpec> {
        self.status.as_ref()
    }

    pub fn db_migrate(&self) -> Option<&CommandSpec> {
        self.db_migrate.as_ref()
    }

    pub fn db_seed(&self) -> Option<&CommandSpec> {
        self.db_seed.as_ref()
    }

    pub fn db_reset(&self) -> Option<&CommandSpec> {
        self.db_reset.as_ref()
    }
}

impl ComposeSpec {
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(|path| path.0.as_path())
    }
}

impl SupabaseProject {
    pub fn config_path(&self) -> &Path {
        &self.config_path.0
    }
}

impl ProjectConfig {
    pub fn from_slice(input: &[u8]) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_slice(input).context("parse project config JSON")?;
        let Some(version) = value.get("version") else {
            return normalize_partial_legacy_supabase(value);
        };
        let version = version
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("project config version must be an integer"))?;
        match version {
            1 => normalize_v1(
                serde_json::from_slice(input).context("parse version 1 project config")?,
            ),
            2 => normalize_v2(
                serde_json::from_slice(input).context("parse version 2 project config")?,
            ),
            version => bail!("unsupported project config version: {version}"),
        }
    }

    pub fn from_discovery_slice(input: &[u8]) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_slice(input).context("parse discovery JSON")?;
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(2) {
            bail!("discovery output must use project config version 2");
        }
        let config = normalize_v2(
            serde_json::from_slice(input).context("parse version 2 discovery output")?,
        )?;
        config.validate_discovered_commands()?;
        Ok(config)
    }

    pub fn load_optional(path: &Path) -> Result<Option<Self>> {
        match fs::read(path) {
            Ok(input) => Self::from_slice(&input)
                .with_context(|| format!("invalid {}", path.display()))
                .map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn load_for(config_root: &Path, checkout_root: &Path) -> Result<Option<Self>> {
        let local = checkout_root.join(".wrt.json");
        match fs::symlink_metadata(&local) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("invalid {}: symlinks are not allowed", local.display())
            }
            Ok(_) => return Self::load_optional(&local),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", local.display()));
            }
        }

        let fallback = config_root.join(".wrt.json");
        if fallback == local {
            return Ok(None);
        }
        Self::load_optional(&fallback)
    }

    pub fn ports(&self) -> &[PortSpec] {
        &self.ports
    }

    pub fn port_stride(&self) -> u16 {
        self.port_stride
    }

    pub fn commands(&self) -> &ProjectCommands {
        &self.commands
    }

    pub fn compose(&self) -> Option<&ComposeSpec> {
        self.compose.as_ref()
    }

    pub fn supabase(&self) -> Option<&SupabaseProject> {
        self.supabase.as_ref()
    }

    pub(crate) fn validate_discovery_paths(&self, repo_root: &Path) -> Result<()> {
        let root = fs::canonicalize(repo_root)
            .with_context(|| format!("resolve discovery root {}", repo_root.display()))?;
        for (name, command) in [
            ("setup", &self.commands.setup),
            ("start", &self.commands.start),
            ("stop", &self.commands.stop),
            ("status", &self.commands.status),
            ("db_migrate", &self.commands.db_migrate),
            ("db_seed", &self.commands.db_seed),
            ("db_reset", &self.commands.db_reset),
        ] {
            if let Some(cwd) = command.as_ref().and_then(|command| command.cwd.as_ref()) {
                validate_discovery_path(&root, &cwd.0, &format!("{name} cwd"), true)?;
            }
        }
        if let Some(compose) = &self.compose {
            for file in &compose.files {
                validate_discovery_path(&root, &file.0, "compose file", false)?;
            }
        }
        if let Some(supabase) = &self.supabase {
            validate_discovery_path(
                &root,
                &supabase.config_path.0,
                "supabase config_path",
                false,
            )?;
        }
        Ok(())
    }

    pub fn discovered_commands(&self) -> Vec<(&'static str, &CommandSpec)> {
        [
            ("setup", self.commands.setup()),
            ("start", self.commands.start()),
            ("stop", self.commands.stop()),
            ("status", self.commands.status()),
            ("db_migrate", self.commands.db_migrate()),
            ("db_seed", self.commands.db_seed()),
            ("db_reset", self.commands.db_reset()),
        ]
        .into_iter()
        .filter_map(|(name, command)| command.map(|command| (name, command)))
        .collect()
    }

    fn validate_discovered_commands(&self) -> Result<()> {
        for (name, command) in self.discovered_commands() {
            let (executable, arguments) = unwrap_discovery_command(&command.argv)
                .with_context(|| format!("invalid discovered {name} command wrapper"))?;
            if uses_command_string(&executable, arguments) {
                bail!("discovered {name} command must not use a shell command-string wrapper");
            }
            if name == "setup"
                && (matches!(executable.as_str(), "rm" | "rmdir" | "truncate")
                    || arguments
                        .iter()
                        .any(|argument| is_destructive_setup_alias(argument)))
            {
                bail!(
                    "discovered setup command looks destructive; setup must not reset or delete data"
                );
            }
        }
        Ok(())
    }
}

fn unwrap_discovery_command(argv: &[String]) -> Result<(String, &[String])> {
    let mut executable = argv
        .first()
        .ok_or_else(|| anyhow::anyhow!("command argv must not be empty"))?;
    let mut arguments = &argv[1..];
    loop {
        if Path::new(executable).is_absolute() {
            bail!("executable must be repository-portable, not absolute");
        }
        let normalized = normalized_executable(executable);
        match normalized.as_str() {
            "env" => {
                let mut index = 0;
                if arguments.first().is_some_and(|argument| argument == "--") {
                    index = 1;
                }
                while arguments.get(index).is_some_and(|argument| {
                    argument
                        .split_once('=')
                        .is_some_and(|(key, _)| is_environment_name(key))
                }) {
                    index += 1;
                }
                if arguments
                    .get(index)
                    .is_some_and(|argument| argument.starts_with('-'))
                {
                    bail!("unsupported env wrapper option");
                }
                executable = arguments
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("env wrapper has no executable"))?;
                arguments = &arguments[index + 1..];
            }
            "busybox" => {
                let mut index = 0;
                if arguments.first().is_some_and(|argument| argument == "--") {
                    index = 1;
                } else if arguments
                    .first()
                    .is_some_and(|argument| argument.starts_with('-'))
                {
                    bail!("unsupported busybox wrapper option");
                }
                executable = arguments
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("busybox wrapper has no applet"))?;
                arguments = &arguments[index + 1..];
            }
            _ => return Ok((normalized, arguments)),
        }
    }
}

fn normalized_executable(executable: &str) -> String {
    let name = Path::new(executable)
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or(executable)
        .to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn is_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn uses_command_string(executable: &str, arguments: &[String]) -> bool {
    match executable {
        "sh" | "ash" | "bash" | "csh" | "dash" | "fish" | "ksh" | "mksh" | "tcsh" | "yash"
        | "zsh" => arguments.iter().any(|argument| {
            let lower = argument.to_ascii_lowercase();
            lower == "--command"
                || lower.starts_with("--command=")
                || lower
                    .strip_prefix('-')
                    .filter(|flags| !flags.starts_with('-'))
                    .is_some_and(|flags| flags.contains('c'))
        }),
        "pwsh" | "powershell" => arguments.iter().any(|argument| {
            let flag = argument.trim_start_matches(['-', '/']).to_ascii_lowercase();
            let flag = flag.split([':', '=']).next().unwrap_or(&flag);
            ["command", "commandwithargs", "encodedcommand"]
                .iter()
                .any(|parameter| !flag.is_empty() && parameter.starts_with(flag))
        }),
        "cmd" => arguments.iter().any(|argument| {
            let flag = argument.to_ascii_lowercase();
            flag.starts_with("/c") || flag.starts_with("/k")
        }),
        _ => false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigV1 {
    version: u32,
    port_block_size: i32,
    #[serde(default)]
    package_manager: Option<PackageManagerV1>,
    #[serde(default)]
    services: Vec<ServiceV1>,
    #[serde(default)]
    database: Option<DatabaseV1>,
    #[serde(default)]
    supabase: Option<SupabaseV1>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManagerV1 {
    #[serde(default)]
    name: String,
    #[serde(default)]
    install_command: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceV1 {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    dev_command: Vec<String>,
    #[serde(default)]
    base_port: Option<i32>,
    #[serde(default)]
    port_env: Option<String>,
    #[serde(default)]
    url_env: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseV1 {
    #[serde(default)]
    detected: bool,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    migrate_command: Option<Vec<String>>,
    #[serde(default)]
    seed_command: Option<Vec<String>>,
    #[serde(default)]
    reset_command: Option<Vec<String>>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupabaseV1 {
    #[serde(default)]
    detected: Option<bool>,
    #[serde(default)]
    config_path: Option<String>,
    #[serde(default)]
    start_command: Option<Vec<String>>,
    #[serde(default)]
    base_ports: Option<BasePortsV1>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BasePortsV1 {
    #[serde(default)]
    api: Option<i32>,
    #[serde(default)]
    db: Option<i32>,
    #[serde(default)]
    shadow_db: Option<i32>,
    #[serde(default)]
    studio: Option<i32>,
    #[serde(default)]
    inbucket: Option<i32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigV2 {
    version: u32,
    port_stride: i32,
    ports: Vec<PortV2>,
    commands: CommandsV2,
    compose: RequiredNullable<ComposeV2>,
    supabase: RequiredNullable<SupabaseV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortV2 {
    key: String,
    base_port: i32,
    outputs: Vec<EnvOutputV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvOutputV2 {
    env: String,
    template: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandsV2 {
    setup: RequiredNullable<CommandV2>,
    start: RequiredNullable<CommandV2>,
    stop: RequiredNullable<CommandV2>,
    status: RequiredNullable<CommandV2>,
    db_migrate: RequiredNullable<CommandV2>,
    db_seed: RequiredNullable<CommandV2>,
    db_reset: RequiredNullable<CommandV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandV2 {
    argv: Vec<String>,
    cwd: RequiredNullable<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RequiredNullable<T> {
    Value(T),
    Null(()),
}

impl<T> RequiredNullable<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Null(()) => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeV2 {
    files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupabaseV2 {
    config_path: String,
}

fn normalize_v1(wire: ConfigV1) -> Result<ProjectConfig> {
    if wire.version != 1 {
        bail!("expected project config version 1");
    }
    let port_stride = validate_stride(wire.port_block_size)?;
    let _ = wire.notes;
    if let Some(package_manager) = wire.package_manager {
        if !matches!(
            package_manager.name.as_str(),
            "pnpm" | "npm" | "yarn" | "bun" | "unknown"
        ) {
            bail!("invalid package manager name {:?}", package_manager.name);
        }
        if package_manager
            .install_command
            .iter()
            .any(|item| item.trim().is_empty())
        {
            bail!("package manager install_command items must not be empty");
        }
        let _ = package_manager.notes;
    }

    let mut ports = Vec::new();
    for service in wire.services {
        let ServiceV1 {
            name,
            kind,
            dev_command,
            base_port,
            port_env,
            url_env,
            notes,
        } = service;
        if name.trim().is_empty() {
            bail!("service name must not be empty");
        }
        validate_argv(dev_command, &format!("service {name:?} dev_command"))?;
        let _ = (kind, notes);
        let Some(base_port) = base_port else {
            if port_env.is_some() || url_env.is_some() {
                bail!("service {name:?} env outputs require base_port");
            }
            continue;
        };
        let base_port = validate_port(base_port)?;
        let key = PortKey(legacy_port_key(&name));
        let mut outputs = Vec::new();
        if let Some(env) = port_env {
            outputs.push(EnvOutput {
                env: EnvName(env),
                template: PortTemplate("{port}".to_string()),
            });
        }
        if let Some(env) = url_env {
            outputs.push(EnvOutput {
                env: EnvName(env),
                template: PortTemplate("http://localhost:{port}".to_string()),
            });
        }
        ports.push(PortSpec {
            key,
            base_port,
            outputs,
        });
    }

    let commands = if let Some(database) = wire.database {
        let has_commands = database.migrate_command.is_some()
            || database.seed_command.is_some()
            || database.reset_command.is_some();
        if !database.detected && has_commands {
            bail!("database commands require detected=true");
        }
        let _ = (database.kind, database.notes);
        ProjectCommands {
            db_migrate: normalize_legacy_command(database.migrate_command, "database migrate")?,
            db_seed: normalize_legacy_command(database.seed_command, "database seed")?,
            db_reset: normalize_legacy_command(database.reset_command, "database reset")?,
            ..Default::default()
        }
    } else {
        ProjectCommands::default()
    };

    let supabase = match wire.supabase {
        Some(supabase) => {
            let SupabaseV1 {
                detected,
                config_path,
                start_command,
                base_ports,
                notes,
            } = supabase;
            let _ = notes;
            let has_start_command = start_command.is_some();
            let has_base_ports = base_ports.is_some();
            if let Some(start_command) = start_command {
                validate_argv(start_command, "supabase start_command")?;
            }
            if let Some(base_ports) = base_ports {
                for port in [
                    base_ports.api,
                    base_ports.db,
                    base_ports.shadow_db,
                    base_ports.studio,
                    base_ports.inbucket,
                ]
                .into_iter()
                .flatten()
                {
                    validate_port(port)?;
                }
            }
            if detected == Some(false)
                && (config_path.is_some() || has_start_command || has_base_ports)
            {
                bail!("supabase details require detected=true");
            }
            if detected.is_none() && config_path.is_none() && (has_start_command || has_base_ports)
            {
                bail!("supabase details require detected or config_path");
            }
            if detected == Some(false) {
                None
            } else if let Some(config_path) = config_path {
                Some(SupabaseProject {
                    config_path: validate_supabase_path(&config_path)?,
                })
            } else if detected == Some(true) {
                Some(SupabaseProject {
                    config_path: validate_supabase_path("supabase/config.toml")?,
                })
            } else {
                None
            }
        }
        None => None,
    };

    validate_config(ProjectConfig {
        port_stride,
        ports,
        commands,
        compose: None,
        supabase,
    })
}

fn normalize_partial_legacy_supabase(value: serde_json::Value) -> Result<ProjectConfig> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("project config must be an object"))?;
    if object.len() != 1 || !object.contains_key("supabase") {
        bail!("project config is missing version");
    }
    let supabase = object["supabase"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("legacy supabase config must be an object"))?;
    if supabase.len() != 1 || !supabase.contains_key("config_path") {
        bail!("project config is missing version");
    }
    let config_path = supabase["config_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("legacy supabase config_path must be a string"))?;
    let config_path = validate_supabase_path(config_path)?;
    Ok(ProjectConfig {
        port_stride: 100,
        ports: Vec::new(),
        commands: ProjectCommands::default(),
        compose: None,
        supabase: Some(SupabaseProject { config_path }),
    })
}

fn normalize_v2(wire: ConfigV2) -> Result<ProjectConfig> {
    if wire.version != 2 {
        bail!("expected project config version 2");
    }
    let port_stride = validate_stride(wire.port_stride)?;

    let ports = wire
        .ports
        .into_iter()
        .map(|port| -> Result<PortSpec> {
            Ok(PortSpec {
                key: PortKey(port.key),
                base_port: validate_port(port.base_port)?,
                outputs: port
                    .outputs
                    .into_iter()
                    .map(|output| EnvOutput {
                        env: EnvName(output.env),
                        template: PortTemplate(output.template),
                    })
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let commands = ProjectCommands {
        setup: normalize_command(wire.commands.setup.into_option(), "setup")?,
        start: normalize_command(wire.commands.start.into_option(), "start")?,
        stop: normalize_command(wire.commands.stop.into_option(), "stop")?,
        status: normalize_command(wire.commands.status.into_option(), "status")?,
        db_migrate: normalize_command(wire.commands.db_migrate.into_option(), "db_migrate")?,
        db_seed: normalize_command(wire.commands.db_seed.into_option(), "db_seed")?,
        db_reset: normalize_command(wire.commands.db_reset.into_option(), "db_reset")?,
    };
    let compose = wire
        .compose
        .into_option()
        .map(|compose| {
            if compose.files.is_empty() {
                bail!("compose.files must not be empty");
            }
            let files = compose
                .files
                .into_iter()
                .map(|file| validate_repo_path(&file, "compose file"))
                .collect::<Result<Vec<_>>>()?;
            Ok(ComposeSpec { files })
        })
        .transpose()?;
    let supabase = wire
        .supabase
        .into_option()
        .map(|supabase| -> Result<SupabaseProject> {
            Ok(SupabaseProject {
                config_path: validate_supabase_path(&supabase.config_path)?,
            })
        })
        .transpose()?;

    validate_config(ProjectConfig {
        port_stride,
        ports,
        commands,
        compose,
        supabase,
    })
}

fn validate_config(config: ProjectConfig) -> Result<ProjectConfig> {
    let mut keys = BTreeSet::new();
    let mut env_names = BTreeSet::new();
    for port in &config.ports {
        if !is_valid_port_key(&port.key.0) {
            bail!("invalid port key {:?}", port.key.0);
        }
        if !keys.insert(&port.key) {
            bail!("duplicate port key {:?}", port.key.0);
        }
        for output in &port.outputs {
            if !is_valid_env_name(&output.env.0) {
                bail!("invalid output env name {:?}", output.env.0);
            }
            if output.env.0.starts_with("WRT_") {
                bail!("output env name {:?} is reserved", output.env.0);
            }
            if !env_names.insert(&output.env) {
                bail!("duplicate output env name {:?}", output.env.0);
            }
            validate_template(&output.template.0)?;
        }
    }
    Ok(config)
}

fn normalize_legacy_command(argv: Option<Vec<String>>, label: &str) -> Result<Option<CommandSpec>> {
    argv.map(|argv| validate_argv(argv, label).map(|argv| CommandSpec { argv, cwd: None }))
        .transpose()
}

fn normalize_command(command: Option<CommandV2>, label: &str) -> Result<Option<CommandSpec>> {
    command
        .map(|command| {
            let argv = validate_argv(command.argv, label)?;
            let cwd = command
                .cwd
                .into_option()
                .map(|cwd| validate_repo_path(&cwd, &format!("{label} cwd")))
                .transpose()?;
            Ok(CommandSpec { argv, cwd })
        })
        .transpose()
}

fn validate_argv(argv: Vec<String>, label: &str) -> Result<Vec<String>> {
    if argv.is_empty() {
        bail!("{label} argv must not be empty");
    }
    if argv.iter().any(|item| item.trim().is_empty()) {
        bail!("{label} argv items must not be empty");
    }
    if argv
        .iter()
        .any(|item| item.chars().any(|character| character.is_control()))
    {
        bail!("{label} argv items must not contain control characters");
    }
    Ok(argv)
}

fn is_destructive_setup_alias(argument: &str) -> bool {
    let normalized = argument.to_ascii_lowercase();
    normalized.split([':', '-', '/']).any(|part| {
        matches!(
            part,
            "reset" | "clean" | "destroy" | "drop" | "delete" | "prune"
        )
    })
}

fn validate_stride(stride: i32) -> Result<u16> {
    if !(1..=65535).contains(&stride) {
        bail!("port stride must be in 1..=65535");
    }
    Ok(stride as u16)
}

fn validate_port(port: i32) -> Result<u16> {
    if !(1..=65535).contains(&port) {
        bail!("port must be in 1..=65535: {port}");
    }
    Ok(port as u16)
}

fn validate_repo_path(input: &str, label: &str) -> Result<RepoRelativePath> {
    if input.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    let path = Path::new(input);
    if path.is_absolute() {
        bail!("{label} must be repo-relative");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("{label} must stay inside the worktree");
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(RepoRelativePath(normalized))
}

fn validate_discovery_path(
    root: &Path,
    relative: &Path,
    label: &str,
    directory: bool,
) -> Result<()> {
    let target = root.join(relative);
    let resolved = fs::canonicalize(&target)
        .with_context(|| format!("{label} does not exist: {}", relative.display()))?;
    if !resolved.starts_with(root) {
        bail!(
            "{label} resolves outside the repository: {}",
            relative.display()
        );
    }
    if directory && !resolved.is_dir() {
        bail!("{label} is not a directory: {}", relative.display());
    }
    if !directory && !resolved.is_file() {
        bail!("{label} is not a file: {}", relative.display());
    }
    Ok(())
}

fn validate_supabase_path(input: &str) -> Result<RepoRelativePath> {
    let path = validate_repo_path(input, "supabase config_path")?;
    if !path.0.ends_with(Path::new("supabase/config.toml")) {
        bail!("supabase config_path must end with supabase/config.toml");
    }
    Ok(path)
}

fn validate_template(template: &str) -> Result<()> {
    let mut rest = template;
    let mut saw_port = false;
    while let Some(open) = rest.find('{') {
        if rest[..open].contains('}') {
            bail!("unmatched output template closing brace");
        }
        let after_open = &rest[open..];
        let Some(close) = after_open.find('}') else {
            bail!("unterminated output template placeholder");
        };
        let placeholder = &after_open[..=close];
        if placeholder != "{port}" {
            bail!("unsupported output template placeholder {placeholder:?}");
        }
        saw_port = true;
        rest = &after_open[close + 1..];
    }
    if rest.contains('}') {
        bail!("unmatched output template closing brace");
    }
    if !saw_port {
        bail!("output template must contain {{port}}");
    }
    Ok(())
}

fn legacy_port_key(name: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !output.is_empty() {
            output.push('-');
            separator = true;
        }
    }
    output.trim_matches('-').to_string()
}

fn is_valid_port_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return false;
    }
    !key.ends_with('-') && !key.contains("--")
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
pub(crate) fn complete_v2_fixture(input: &str) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_str(input).unwrap();
    let root = value.as_object_mut().unwrap();
    root.entry("ports").or_insert_with(|| serde_json::json!([]));
    for port in root["ports"].as_array_mut().unwrap() {
        port.as_object_mut()
            .unwrap()
            .entry("outputs")
            .or_insert_with(|| serde_json::json!([]));
    }
    let commands = root
        .entry("commands")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .unwrap();
    for name in [
        "setup",
        "start",
        "stop",
        "status",
        "db_migrate",
        "db_seed",
        "db_reset",
    ] {
        let command = commands.entry(name).or_insert(serde_json::Value::Null);
        if let Some(command) = command.as_object_mut() {
            command.entry("cwd").or_insert(serde_json::Value::Null);
        }
    }
    root.entry("compose").or_insert(serde_json::Value::Null);
    root.entry("supabase").or_insert(serde_json::Value::Null);
    serde_json::to_vec(&value).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn normalizes_v1_and_v2_to_the_same_model() {
        let v1 = br#"{
          "version": 1,
          "port_block_size": 100,
          "services": [{
            "name": "Core API",
            "dev_command": ["pnpm", "dev"],
            "base_port": 3000,
            "port_env": "CORE_API_PORT",
            "url_env": "CORE_API_URL"
          }],
          "database": {"detected": true, "reset_command": ["pnpm", "db:reset"]},
          "supabase": {"config_path": "infra/supabase/config.toml"}
        }"#;
        let v2 = complete_v2_fixture(
            r#"{
          "version": 2,
          "port_stride": 100,
          "ports": [{
            "key": "core-api",
            "base_port": 3000,
            "outputs": [
              {"env": "CORE_API_PORT", "template": "{port}"},
              {"env": "CORE_API_URL", "template": "http://localhost:{port}"}
            ]
          }],
          "commands": {"db_reset": {"argv": ["pnpm", "db:reset"]}},
          "supabase": {"config_path": "infra/supabase/config.toml"}
        }"#,
        );

        assert_eq!(
            ProjectConfig::from_slice(v1).unwrap(),
            ProjectConfig::from_slice(&v2).unwrap()
        );
    }

    #[test]
    fn supports_minimal_v1_and_partial_supabase() {
        let minimal = br#"{"version":1,"port_block_size":100}"#;
        let config = ProjectConfig::from_slice(minimal).unwrap();
        assert!(config.ports.is_empty());
        assert_eq!(config.supabase, None);

        let partial = br#"{
          "version": 1,
          "port_block_size": 100,
          "supabase": {"config_path": "services/alt/supabase/config.toml"}
        }"#;
        assert_eq!(
            ProjectConfig::from_slice(partial)
                .unwrap()
                .supabase
                .unwrap()
                .config_path()
                .to_string_lossy(),
            "services/alt/supabase/config.toml"
        );

        let exact_legacy = br#"{"supabase":{"config_path":"apps/api/supabase/config.toml"}}"#;
        assert_eq!(
            ProjectConfig::from_slice(exact_legacy)
                .unwrap()
                .supabase
                .unwrap()
                .config_path()
                .to_string_lossy(),
            "apps/api/supabase/config.toml"
        );
    }

    #[test]
    fn rejects_invalid_v2_boundaries() {
        let cases = [
            (r#"{"version":3}"#, "unsupported project config version"),
            (r#"{"version":2,"port_stride":0}"#, "port stride must be"),
            (
                r#"{"version":2,"port_stride":100,"ports":[{"key":"Bad Key","base_port":3000}]}"#,
                "invalid port key",
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[{"key":"web","base_port":70000}]}"#,
                "port must be",
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[{"key":"web","base_port":3000,"outputs":[{"env":"WRT_NAME","template":"{port}"}]}]}"#,
                "is reserved",
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[{"key":"web","base_port":3000,"outputs":[{"env":"PORT","template":"{port}"}]},{"key":"api","base_port":3001,"outputs":[{"env":"PORT","template":"{port}"}]}]}"#,
                "duplicate output env name",
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[{"key":"web","base_port":3000,"outputs":[{"env":"PORT","template":"{host}"}]}]}"#,
                "unsupported output template placeholder",
            ),
            (
                r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":[]}}}"#,
                "argv must not be empty",
            ),
            (
                r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["pnpm","" ]}}}"#,
                "argv items must not be empty",
            ),
            (
                r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["pnpm"],"cwd":"../outside"}}}"#,
                "must stay inside the worktree",
            ),
            (
                r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["pnpm"],"cwd":"/tmp/outside"}}}"#,
                "must be repo-relative",
            ),
            (
                r#"{"version":2,"port_stride":100,"compose":{"files":["../compose.yaml"]}}"#,
                "must stay inside the worktree",
            ),
            (
                r#"{"version":2,"port_stride":100,"supabase":{"config_path":"compose.yaml"}}"#,
                "must end with supabase/config.toml",
            ),
        ];

        for (input, expected) in cases {
            let input = complete_v2_fixture(input);
            let error = ProjectConfig::from_slice(&input).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    #[test]
    fn rejects_missing_schema_required_v2_fields() {
        let cases = [
            (
                r#"{"version":2,"port_stride":100}"#,
                Some("missing field `ports`"),
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[],"commands":{},"compose":null,"supabase":null}"#,
                None,
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000}],"commands":{"setup":null,"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},"compose":null,"supabase":null}"#,
                Some("missing field `outputs`"),
            ),
            (
                r#"{"version":2,"port_stride":100,"ports":[],"commands":{"setup":{"argv":["pnpm","setup"]},"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},"compose":null,"supabase":null}"#,
                None,
            ),
        ];

        for (input, expected) in cases {
            let error = ProjectConfig::from_slice(input.as_bytes()).unwrap_err();
            if let Some(expected) = expected {
                assert!(format!("{error:#}").contains(expected), "{error:#}");
            }
        }
    }

    #[test]
    fn validates_declared_discovery_paths_in_the_current_checkout() {
        let repo = TempDir::new().unwrap();
        fs::create_dir_all(repo.path().join("apps/api")).unwrap();
        fs::create_dir_all(repo.path().join("infra/supabase")).unwrap();
        fs::write(repo.path().join("compose.yaml"), "services: {}\n").unwrap();
        fs::write(
            repo.path().join("infra/supabase/config.toml"),
            "project_id = \"test\"\n",
        )
        .unwrap();
        let input = complete_v2_fixture(
            r#"{
              "version":2,
              "port_stride":100,
              "commands":{"setup":{"argv":["pnpm","setup"],"cwd":"apps/api"}},
              "compose":{"files":["compose.yaml"]},
              "supabase":{"config_path":"infra/supabase/config.toml"}
            }"#,
        );
        let config = ProjectConfig::from_slice(&input).unwrap();

        config.validate_discovery_paths(repo.path()).unwrap();

        fs::remove_file(repo.path().join("compose.yaml")).unwrap();
        let error = config
            .validate_discovery_paths(repo.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("compose file does not exist"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_path_validation_rejects_symlinks_outside_the_checkout() {
        use std::os::unix::fs::symlink;

        let repo = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("compose.yaml"), "services: {}\n").unwrap();
        symlink(
            outside.path().join("compose.yaml"),
            repo.path().join("compose.yaml"),
        )
        .unwrap();
        let input = complete_v2_fixture(
            r#"{
              "version":2,
              "port_stride":100,
              "compose":{"files":["compose.yaml"]}
            }"#,
        );
        let config = ProjectConfig::from_slice(&input).unwrap();

        let error = config
            .validate_discovery_paths(repo.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("resolves outside the repository"), "{error}");
    }

    #[test]
    fn rejects_malformed_v1_outside_the_narrow_legacy_forms() {
        let cases = [
            (
                r#"{"supabase":{"config_path":"supabase/config.toml"},"services":[]}"#,
                "missing version",
            ),
            (
                r#"{"version":1,"port_block_size":100,"services":[{"name":"web","dev_command":[],"base_port":3000}]}"#,
                "dev_command argv must not be empty",
            ),
            (
                r#"{"version":1,"port_block_size":100,"database":{"detected":false,"reset_command":["pnpm","db:reset"]}}"#,
                "database commands require detected=true",
            ),
            (
                r#"{"version":1,"port_block_size":100,"supabase":{"detected":false,"config_path":"supabase/config.toml"}}"#,
                "supabase details require detected=true",
            ),
            (
                r#"{"version":1,"port_block_size":100,"unknown":true}"#,
                "unknown field",
            ),
        ];

        for (input, expected) in cases {
            let error = ProjectConfig::from_slice(input.as_bytes()).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    #[test]
    fn invalid_checkout_config_does_not_fall_back() {
        let root = TempDir::new().unwrap();
        let checkout = root.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        fs::write(
            root.path().join(".wrt.json"),
            r#"{"version":1,"port_block_size":100}"#,
        )
        .unwrap();
        fs::write(checkout.join(".wrt.json"), "not json").unwrap();

        let error = ProjectConfig::load_for(root.path(), &checkout).unwrap_err();
        assert!(error.to_string().contains("checkout/.wrt.json"));
    }

    #[test]
    fn discovery_accepts_only_v2_and_rejects_unsafe_generated_commands() {
        let legacy = br#"{"version":1,"port_block_size":100}"#;
        assert!(ProjectConfig::from_discovery_slice(legacy).is_err());

        for input in [
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["sh","-c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["bash","-lc","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["zsh","-ec","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["dash","-c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["ksh","-lc","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["PowerShell","-Co","Write-Host pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["pwsh","-ENC","ZQBjAGgAbwA="]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["env","sh","-c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["env","dash","-c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["env","SAFE=1","rm","-r","build"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["busybox","sh","-c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["busybox","ash","-c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["busybox","rm","-r","build"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["PowerShell.exe","-Command","Write-Host pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["cmd.exe","/c","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["cmd.exe","/kstart","echo pwned"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"start":{"argv":["/usr/bin/pnpm","start"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["pnpm","db:reset"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["rm","-r","build"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["rmdir","build"]}}}"#,
            r#"{"version":2,"port_stride":100,"commands":{"setup":{"argv":["truncate","-s","0","db"]}}}"#,
        ] {
            let input = complete_v2_fixture(input);
            assert!(ProjectConfig::from_discovery_slice(&input).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_local_config_is_invalid_and_never_falls_back() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let checkout = root.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        fs::write(
            root.path().join(".wrt.json"),
            complete_v2_fixture(r#"{"version":2,"port_stride":100}"#),
        )
        .unwrap();
        symlink("missing.json", checkout.join(".wrt.json")).unwrap();

        let error = ProjectConfig::load_for(root.path(), &checkout).unwrap_err();
        assert!(
            error.to_string().contains("symlinks are not allowed"),
            "{error}"
        );
    }
}
