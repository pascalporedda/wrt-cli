use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::gitx::Repo;
use crate::project::ProjectConfig;
use crate::state::{Allocation, State};
use crate::supabase;
use crate::util::sh_quote;
use crate::worktree;

const SUPABASE_BLOCK_START: &str = "# >>> wrt supabase (generated)";
const SUPABASE_BLOCK_END: &str = "# <<< wrt supabase";

#[derive(Clone, Debug, Default)]
pub struct ResolvedEnvironment {
    values: BTreeMap<String, String>,
    sources: BTreeMap<String, EnvSource>,
}

#[derive(Clone, Debug)]
enum EnvSource {
    Wrt,
    Compose,
    ProjectPort(String),
    ProjectOutput(String),
    Supabase(String),
}

impl fmt::Display for EnvSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wrt => write!(formatter, "wrt built-ins"),
            Self::Compose => write!(formatter, "the Compose project name"),
            Self::ProjectPort(key) => write!(formatter, "project port {key:?}"),
            Self::ProjectOutput(key) => write!(formatter, "project port {key:?} output"),
            Self::Supabase(key) => write!(formatter, "Supabase status field {key:?}"),
        }
    }
}

impl ResolvedEnvironment {
    pub fn build(
        repo: &Repo,
        state: &State,
        alloc: &Allocation,
        project: Option<&ProjectConfig>,
    ) -> Result<Self> {
        Self::build_inner(repo, state, alloc, project, true)
    }

    pub fn build_before_setup(
        repo: &Repo,
        state: &State,
        alloc: &Allocation,
        project: Option<&ProjectConfig>,
    ) -> Result<Self> {
        Self::build_inner(repo, state, alloc, project, false)
    }

    fn build_inner(
        repo: &Repo,
        state: &State,
        alloc: &Allocation,
        project: Option<&ProjectConfig>,
        include_supabase_status: bool,
    ) -> Result<Self> {
        require_project_port_shape(project, alloc)?;

        let mut environment = Self::default();
        let wt_path = Path::new(&alloc.path);
        environment.insert_checked(EnvSource::Wrt, "WRT_NAME", alloc.name.clone())?;
        environment.insert_checked(EnvSource::Wrt, "WRT_BRANCH", alloc.branch.clone())?;
        environment.insert_checked(EnvSource::Wrt, "WRT_PORT_BLOCK", alloc.block.to_string())?;
        environment.insert_checked(EnvSource::Wrt, "WRT_PORT_OFFSET", alloc.offset.to_string())?;
        environment.insert_checked(
            EnvSource::Wrt,
            "WRT_ROOT",
            repo.managed_root.to_string_lossy().to_string(),
        )?;
        environment.insert_checked(
            EnvSource::Wrt,
            "WRT_WORKTREE_PATH",
            wt_path.to_string_lossy().to_string(),
        )?;
        environment.insert_checked(
            EnvSource::Wrt,
            "WRT_MAIN_PATH",
            repo.main_worktree.to_string_lossy().to_string(),
        )?;
        environment.insert_checked(
            EnvSource::Compose,
            "COMPOSE_PROJECT_NAME",
            compose_project_name(repo, alloc),
        )?;

        if let Some(project) = project {
            add_project_vars(&mut environment, project, alloc)?;
        }

        if let Some((owner, target)) = supabase::allocation_target(state, alloc)? {
            if include_supabase_status {
                let status = supabase::status_env(Path::new(&owner.path), &target)?;
                add_supabase_vars(&mut environment, &status)?;
            }
            environment.insert_checked(EnvSource::Wrt, "WRT_SUPABASE_OWNER", owner.name.clone())?;
        }

        Ok(environment)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn apply_to(&self, command: &mut Command) {
        command.envs(&self.values);
    }

    fn insert_checked(
        &mut self,
        source: EnvSource,
        name: impl Into<String>,
        value: String,
    ) -> Result<()> {
        let name = name.into();
        if let Some(existing) = self.values.get(&name) {
            if existing == &value {
                return Ok(());
            }
            let existing_source = &self.sources[&name];
            bail!("environment variable {name:?} conflicts between {existing_source} and {source}");
        }
        self.sources.insert(name.clone(), source);
        self.values.insert(name, value);
        Ok(())
    }
}

fn require_project_port_shape(project: Option<&ProjectConfig>, alloc: &Allocation) -> Result<()> {
    let configured = project
        .into_iter()
        .flat_map(ProjectConfig::ports)
        .map(|spec| spec.key().as_str())
        .collect::<BTreeSet<_>>();
    let allocated = alloc
        .ports
        .keys()
        .map(|key| key.as_str())
        .filter(|key| !key.starts_with("supabase."))
        .collect::<BTreeSet<_>>();
    if configured == allocated {
        return Ok(());
    }

    let added = configured
        .difference(&allocated)
        .copied()
        .collect::<Vec<_>>();
    let removed = allocated
        .difference(&configured)
        .copied()
        .collect::<Vec<_>>();
    bail!(
        "project port keys changed for worktree {:?}; added since allocation: {added:?}; removed since allocation: {removed:?}; recreate the worktree with `wrt rm {}` and `wrt new {}`",
        alloc.name,
        alloc.name,
        alloc.name
    )
}

pub fn sync_env_files(
    state: &State,
    wt_path: &Path,
    alloc: &Allocation,
    environment: &ResolvedEnvironment,
) -> Result<()> {
    write_wrt_env_file(wt_path, environment)?;

    let supabase_vars: BTreeMap<String, String> = environment
        .iter()
        .filter(|(key, _)| is_supabase_env_name(key))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    let target = supabase::allocation_target(state, alloc)?
        .map(|(_, target)| target)
        .or_else(|| {
            state
                .root
                .as_ref()
                .and_then(|root| root.supabase_config_path.as_deref())
                .and_then(|path| supabase::Target::from_config_path(path).ok())
        });

    let mut env_paths = vec![(wt_path.join(".env"), std::path::PathBuf::from(".env"))];
    if let Some(target) = target {
        let relative = target.relative_workdir().join(".env");
        let nested = wt_path.join(&relative);
        if nested != env_paths[0].0 {
            env_paths.push((nested, relative));
        }
    }

    for (path, relative) in env_paths {
        if supabase_vars.is_empty() {
            remove_managed_block(&path)?;
            remove_managed_block(&path.with_file_name(".env.local"))?;
        } else {
            let output_path = env_output_path(wt_path, &path, &relative)?;
            if output_path != path {
                remove_managed_block(&path)?;
            }
            write_managed_block(&output_path, &supabase_vars)?;
        }
    }
    Ok(())
}

fn write_wrt_env_file(wt_path: &Path, environment: &ResolvedEnvironment) -> Result<()> {
    let p = wt_path.join(".wrt.env");
    let mut out = "# Generated by wrt. Safe to edit; re-running wrt may overwrite.\n".to_string();
    for (k, v) in environment.iter() {
        out.push_str(k);
        out.push('=');
        out.push_str(&sh_quote(v));
        out.push('\n');
    }
    fs::write(&p, out.as_bytes()).with_context(|| format!("write {}", p.display()))?;
    Ok(())
}

pub fn print_exports(environment: &ResolvedEnvironment) {
    for (k, v) in environment.iter() {
        println!("export {k}={}", sh_quote(v));
    }
}

fn add_supabase_vars(
    environment: &mut ResolvedEnvironment,
    status: &BTreeMap<String, String>,
) -> Result<()> {
    copy_status_var(environment, status, "API_URL", "SUPABASE_URL")?;
    copy_status_var(environment, status, "ANON_KEY", "SUPABASE_ANON_KEY")?;
    copy_status_var(
        environment,
        status,
        "SERVICE_ROLE_KEY",
        "SUPABASE_SERVICE_ROLE_KEY",
    )?;
    copy_status_var(environment, status, "JWT_SECRET", "SUPABASE_JWT_SECRET")?;
    copy_status_var(environment, status, "DB_URL", "SUPABASE_DB_URL")?;
    copy_status_var(environment, status, "DB_URL", "DATABASE_URL")?;
    copy_status_var(environment, status, "STUDIO_URL", "SUPABASE_STUDIO_URL")?;
    copy_status_var(environment, status, "GRAPHQL_URL", "SUPABASE_GRAPHQL_URL")?;
    copy_status_var(environment, status, "INBUCKET_URL", "SUPABASE_INBUCKET_URL")?;
    Ok(())
}

fn copy_status_var(
    environment: &mut ResolvedEnvironment,
    status: &BTreeMap<String, String>,
    source: &str,
    destination: &str,
) -> Result<()> {
    if let Some(value) = status.get(source) {
        environment.insert_checked(
            EnvSource::Supabase(source.to_string()),
            destination,
            value.clone(),
        )?;
    }
    Ok(())
}

fn env_output_path(wt_path: &Path, path: &Path, relative: &Path) -> Result<std::path::PathBuf> {
    if !is_tracked(wt_path, relative)? {
        return Ok(path.to_path_buf());
    }

    let local_path = path.with_file_name(".env.local");
    let local_relative = relative.with_file_name(".env.local");
    if is_tracked(wt_path, &local_relative)? {
        anyhow::bail!(
            "both {} and {} are tracked; refusing to write Supabase credentials",
            relative.display(),
            local_relative.display()
        );
    }
    Ok(local_path)
}

fn is_tracked(wt_path: &Path, relative: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .current_dir(wt_path)
        .output()
        .with_context(|| format!("check whether {} is tracked", relative.display()))?;
    Ok(output.status.success())
}

fn is_supabase_env_name(key: &str) -> bool {
    key.starts_with("SUPABASE_") || key == "DATABASE_URL"
}

fn write_managed_block(path: &Path, vars: &BTreeMap<String, String>) -> Result<()> {
    let mut input = match fs::read_to_string(path) {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    remove_block_from_string(&mut input)?;
    if !input.is_empty() && !input.ends_with('\n') {
        input.push('\n');
    }
    if !input.is_empty() && !input.ends_with("\n\n") {
        input.push('\n');
    }
    input.push_str(SUPABASE_BLOCK_START);
    input.push('\n');
    for (key, value) in vars {
        input.push_str(key);
        input.push('=');
        input.push_str(&sh_quote(value));
        input.push('\n');
    }
    input.push_str(SUPABASE_BLOCK_END);
    input.push('\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::write(path, input).with_context(|| format!("write {}", path.display()))
}

fn remove_managed_block(path: &Path) -> Result<()> {
    let mut input = match fs::read_to_string(path) {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if !remove_block_from_string(&mut input)? {
        return Ok(());
    }
    fs::write(path, input).with_context(|| format!("write {}", path.display()))
}

fn remove_block_from_string(input: &mut String) -> Result<bool> {
    let Some(start) = input.find(SUPABASE_BLOCK_START) else {
        return Ok(false);
    };
    let rest = &input[start..];
    let end = rest
        .find(SUPABASE_BLOCK_END)
        .ok_or_else(|| anyhow::anyhow!("unterminated wrt Supabase block"))?;
    let mut end = start + end + SUPABASE_BLOCK_END.len();
    if input.as_bytes().get(end) == Some(&b'\r') {
        end += 1;
    }
    if input.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    input.replace_range(start..end, "");
    while input.ends_with("\n\n") {
        input.pop();
    }
    Ok(true)
}

fn add_project_vars(
    environment: &mut ResolvedEnvironment,
    config: &ProjectConfig,
    allocation: &Allocation,
) -> Result<()> {
    for spec in config.ports() {
        let key = spec.key().as_str();
        let port = allocation.ports[spec.key()];

        let port_key = key.replace('-', "_").to_ascii_uppercase();
        environment.insert_checked(
            EnvSource::ProjectPort(key.to_string()),
            format!("WRT_SERVICE_{port_key}_PORT"),
            port.to_string(),
        )?;
        environment.insert_checked(
            EnvSource::ProjectPort(key.to_string()),
            format!("WRT_SERVICE_{port_key}_URL"),
            format!("http://localhost:{port}"),
        )?;

        for output in spec.outputs() {
            environment.insert_checked(
                EnvSource::ProjectOutput(key.to_string()),
                output.env(),
                output.render(port),
            )?;
        }
    }
    Ok(())
}

fn compose_project_name(repo: &Repo, alloc: &Allocation) -> String {
    let root_name = repo
        .managed_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let full = worktree::slug(&format!("wrt-{root_name}-{}", alloc.name));
    if full.len() <= 63 {
        return full;
    }

    let hash = stable_hash(&full);
    let prefix = full[..54].trim_end_matches('-');
    format!("{prefix}-{hash:08x}")
}

fn stable_hash(value: &str) -> u32 {
    let mut hash = 2_166_136_261_u32;
    for byte in value.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitx::Repo;
    use crate::state::{Allocation, State, SupabaseAllocation, project_port_assignments};
    use tempfile::TempDir;

    fn repo(root: &Path) -> Repo {
        Repo {
            root: root.to_path_buf(),
            common_dir: root.join(".git"),
            managed_root: root.to_path_buf(),
            invocation_root: Some(root.to_path_buf()),
            config_root: root.to_path_buf(),
            main_worktree: root.to_path_buf(),
            worktree_parent: root.to_path_buf(),
        }
    }

    fn alloc(path: &Path) -> Allocation {
        Allocation {
            generation_id: crate::state::AllocationGeneration::new(),
            name: "x".into(),
            branch: "x".into(),
            path: path.to_string_lossy().to_string(),
            block: 1,
            offset: 100,
            ports: Default::default(),
            status: "active".into(),
            created_at: "now".into(),
            supabase: SupabaseAllocation::None,
            setup: crate::state::AllocationSetup::default(),
        }
    }

    fn config(input: &str) -> ProjectConfig {
        let input = crate::project::complete_v2_fixture(input);
        ProjectConfig::from_slice(&input).unwrap()
    }

    #[test]
    fn renders_eln_outputs_from_persisted_assignments() {
        let td = TempDir::new().unwrap();
        let project = config(
            r#"{
              "version": 2,
              "port_stride": 100,
              "ports": [
                {"key":"postgres","base_port":5432,"outputs":[{"env":"DATABASE_URL","template":"postgresql://postgres:postgres@localhost:{port}/eln"}]},
                {"key":"redis","base_port":6379,"outputs":[{"env":"REDIS_URL","template":"redis://localhost:{port}"}]},
                {"key":"rabbitmq","base_port":5672,"outputs":[{"env":"AMQP_URL","template":"amqp://localhost:{port}"}]},
                {"key":"core-api","base_port":3000,"outputs":[{"env":"CORE_API_PORT","template":"{port}"},{"env":"CORE_API_URL","template":"http://localhost:{port}/api/v1"}]},
                {"key":"oauth","base_port":8181,"outputs":[{"env":"OAUTH_CALLBACK_URL","template":"http://localhost:{port}/callback"}]}
              ]
            }"#,
        );
        let mut allocation = alloc(td.path());
        for spec in project.ports() {
            let port = match spec.key().as_str() {
                "postgres" => 15432,
                "redis" => 16379,
                "rabbitmq" => 15672,
                "core-api" => 13000,
                "oauth" => 18181,
                _ => unreachable!(),
            };
            allocation.ports.insert(spec.key().clone(), port);
        }

        let environment = ResolvedEnvironment::build(
            &repo(td.path()),
            &State::empty(),
            &allocation,
            Some(&project),
        )
        .unwrap();
        assert_eq!(environment.values["WRT_NAME"], "x");
        assert_eq!(environment.values["WRT_PORT_OFFSET"], "100");
        assert_eq!(
            environment.values["DATABASE_URL"],
            "postgresql://postgres:postgres@localhost:15432/eln"
        );
        assert_eq!(environment.values["REDIS_URL"], "redis://localhost:16379");
        assert_eq!(environment.values["AMQP_URL"], "amqp://localhost:15672");
        assert_eq!(environment.values["CORE_API_PORT"], "13000");
        assert_eq!(
            environment.values["CORE_API_URL"],
            "http://localhost:13000/api/v1"
        );
        assert_eq!(
            environment.values["OAUTH_CALLBACK_URL"],
            "http://localhost:18181/callback"
        );
        assert_eq!(environment.values["WRT_SERVICE_CORE_API_PORT"], "13000");
    }

    #[test]
    fn requires_exact_generic_assignment_keys_and_ignores_supabase_claims() {
        let td = TempDir::new().unwrap();
        let original =
            config(r#"{"version":2,"port_stride":100,"ports":[{"key":"web","base_port":3000}]}"#);
        let changed =
            config(r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3001}]}"#);
        let mut allocation = alloc(td.path());
        allocation.ports = project_port_assignments(&original, allocation.offset).unwrap();
        let supabase_key = serde_json::from_str(r#""supabase.api.port""#).unwrap();
        allocation.ports.insert(supabase_key, 54321);

        ResolvedEnvironment::build(
            &repo(td.path()),
            &State::empty(),
            &allocation,
            Some(&original),
        )
        .unwrap();
        let error = ResolvedEnvironment::build(
            &repo(td.path()),
            &State::empty(),
            &allocation,
            Some(&changed),
        )
        .unwrap_err();
        let error = error.to_string();
        assert!(
            error.contains("added since allocation: [\"api\"]"),
            "{error}"
        );
        assert!(
            error.contains("removed since allocation: [\"web\"]"),
            "{error}"
        );
        assert!(error.contains("recreate the worktree"), "{error}");
    }

    #[test]
    fn checked_insertion_rejects_conflicts_and_allows_equal_values() {
        let mut environment = ResolvedEnvironment::default();
        environment
            .insert_checked(
                EnvSource::ProjectOutput("postgres".into()),
                "DATABASE_URL",
                "postgresql://project".into(),
            )
            .unwrap();
        environment
            .insert_checked(
                EnvSource::ProjectOutput("replica".into()),
                "DATABASE_URL",
                "postgresql://project".into(),
            )
            .unwrap();

        let error = environment
            .insert_checked(
                EnvSource::Supabase("DB_URL".into()),
                "DATABASE_URL",
                "postgresql://supabase".into(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("project port \"postgres\" output"),
            "{error}"
        );
        assert!(
            error.contains("Supabase status field \"DB_URL\""),
            "{error}"
        );
    }

    #[test]
    fn truncated_compose_names_include_a_stable_hash() {
        let td = TempDir::new().unwrap();
        let mut first = alloc(td.path());
        first.name = format!("feature-{}-one", "a".repeat(80));
        let mut second = first.clone();
        second.name = format!("feature-{}-two", "a".repeat(80));

        let first_name = compose_project_name(&repo(td.path()), &first);
        let second_name = compose_project_name(&repo(td.path()), &second);
        assert_eq!(first_name.len(), 63);
        assert_eq!(second_name.len(), 63);
        assert_ne!(first_name, second_name);
        assert_eq!(first_name, compose_project_name(&repo(td.path()), &first));
    }

    #[test]
    fn managed_supabase_env_block_is_idempotent_and_removable() {
        let td = TempDir::new().unwrap();
        let path = td.path().join(".env");
        fs::write(&path, "EXISTING=value\n").unwrap();
        let vars = BTreeMap::from([
            (
                "SUPABASE_URL".to_string(),
                "http://127.0.0.1:54321".to_string(),
            ),
            ("SUPABASE_ANON_KEY".to_string(), "anon".to_string()),
        ]);

        write_managed_block(&path, &vars).unwrap();
        write_managed_block(&path, &vars).unwrap();
        let output = fs::read_to_string(&path).unwrap();
        assert_eq!(output.matches(SUPABASE_BLOCK_START).count(), 1);
        assert!(output.contains("EXISTING=value"));
        assert!(output.contains("SUPABASE_ANON_KEY='anon'"));

        remove_managed_block(&path).unwrap();
        let output = fs::read_to_string(path).unwrap();
        assert_eq!(output, "EXISTING=value\n");
    }
}
