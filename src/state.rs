use anyhow::{Context, Result, anyhow, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::gitx::Repo;
use crate::project::{PortKey, ProjectConfig};

const STATE_DIR_NAME: &str = ".wrt";
const STATE_FILE_NAME: &str = "state.json";
const STATE_LOCK_FILE_NAME: &str = "state.lock";
const LIFECYCLE_LOCK_DIR_NAME: &str = "lifecycle";
const CURRENT_VER: i32 = 4;

pub const LAYOUT_MANAGED_ROOT: &str = "managed-root";
pub type PortAssignments = BTreeMap<PortKey, u16>;

#[derive(Clone, Debug)]
pub struct ReservationRequest {
    port_stride: u16,
    generic_ports: PortAssignments,
    isolated_supabase_ports: PortAssignments,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub block: i32,
    pub offset: i32,
    pub ports: PortAssignments,
}

impl ReservationRequest {
    pub fn new(project: Option<&ProjectConfig>, isolated_supabase_ports: PortAssignments) -> Self {
        let port_stride = project.map(ProjectConfig::port_stride).unwrap_or(100);
        let generic_ports = project
            .into_iter()
            .flat_map(ProjectConfig::ports)
            .map(|spec| (spec.key().clone(), spec.base_port()))
            .collect();
        Self {
            port_stride,
            generic_ports,
            isolated_supabase_ports,
        }
    }

    pub fn compose_probe(project: &ProjectConfig, allocation: &Allocation) -> Result<Self> {
        let isolated_supabase_ports = allocation
            .ports
            .iter()
            .filter(|(key, _)| key.as_str().starts_with("supabase."))
            .map(|(key, port)| {
                let base = i32::from(*port)
                    .checked_sub(allocation.offset)
                    .filter(|base| (1..=65535).contains(base))
                    .ok_or_else(|| {
                        anyhow!(
                            "cannot reconstruct base port for persisted claim {:?}",
                            key.as_str()
                        )
                    })?;
                Ok((key.clone(), base as u16))
            })
            .collect::<Result<PortAssignments>>()?;
        Ok(Self::new(Some(project), isolated_supabase_ports))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AllocationGeneration(Uuid);

impl AllocationGeneration {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: i32,
    #[serde(default)]
    pub root: Option<RootState>,
    #[serde(default)]
    pub allocations: BTreeMap<String, Allocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RootState {
    pub layout: String,
    #[serde(rename = "managedRoot")]
    pub managed_root: String,
    #[serde(rename = "gitCommonDir")]
    pub git_common_dir: String,
    #[serde(rename = "mainWorktree")]
    pub main_worktree: String,
    #[serde(rename = "worktreesPath")]
    pub worktrees_path: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(default, rename = "supabaseConfigPath")]
    pub supabase_config_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allocation {
    #[serde(rename = "generationId")]
    pub generation_id: AllocationGeneration,
    pub name: String,
    pub branch: String,
    pub path: String,
    pub block: i32,
    pub offset: i32,
    pub ports: PortAssignments,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(default)]
    pub supabase: SupabaseAllocation,
    #[serde(default)]
    pub setup: AllocationSetup,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AllocationSetup {
    pub install: String,
    pub db: String,
}

impl Default for AllocationSetup {
    fn default() -> Self {
        Self {
            install: "auto".to_string(),
            db: "auto".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SupabaseAllocation {
    #[default]
    None,
    Owned {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "configPath")]
        config_path: String,
    },
    Shared {
        owner: String,
    },
}

#[derive(Clone, Debug)]
pub struct StateStore {
    common_dir: PathBuf,
    config_root: PathBuf,
    managed_root: PathBuf,
    main_worktree: PathBuf,
    worktree_parent: PathBuf,
}

impl StateStore {
    pub fn new(repo: &Repo) -> Self {
        Self {
            common_dir: repo.common_dir.clone(),
            config_root: repo.config_root.clone(),
            managed_root: repo.managed_root.clone(),
            main_worktree: repo.main_worktree.clone(),
            worktree_parent: repo.worktree_parent.clone(),
        }
    }

    pub fn read(&self) -> Result<State> {
        let _lock = StateLock::acquire(&self.common_dir)?;
        let (state, migrated) = self.load_locked()?;
        if migrated {
            self.validate_external(&state)?;
            self.save_locked(&state)?;
        } else {
            self.validate_external(&state)?;
        }
        Ok(state)
    }

    pub fn update<T>(&self, mutate: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let _lock = StateLock::acquire(&self.common_dir)?;
        let (mut state, _) = self.load_locked()?;
        let result = mutate(&mut state)?;
        state.version = CURRENT_VER;
        validate_state(&state)?;
        self.validate_external(&state)?;
        self.save_locked(&state)?;
        Ok(result)
    }

    pub fn lock_allocation(&self, allocation_name: &str) -> Result<AllocationLock> {
        self.lock_allocation_with(allocation_name, AllocationLockMode::Exclusive)
    }

    pub fn lock_allocation_shared(&self, allocation_name: &str) -> Result<AllocationLock> {
        self.lock_allocation_with(allocation_name, AllocationLockMode::Shared)
    }

    pub fn lock_allocation_read(
        &self,
        snapshot: &State,
        allocation_name: &str,
    ) -> Result<AllocationReadGuard> {
        let expected = snapshot
            .allocations
            .get(allocation_name)
            .ok_or_else(|| anyhow!("unknown worktree: {allocation_name:?}"))?;
        let expected_generation = expected.generation_id;
        let expected_supabase = expected.supabase.clone();
        let expected_owner = match &expected.supabase {
            SupabaseAllocation::Shared { owner } => Some((
                owner.clone(),
                snapshot
                    .allocations
                    .get(owner)
                    .ok_or_else(|| anyhow!("shared Supabase owner is missing: {owner:?}"))?
                    .generation_id,
            )),
            _ => None,
        };
        let selected_lock = self.lock_allocation_shared(allocation_name)?;
        let owner_lock = expected_owner
            .as_ref()
            .map(|(owner, _)| self.lock_allocation_shared(owner))
            .transpose()?;
        let state = self.read()?;
        state
            .allocations
            .get(allocation_name)
            .filter(|allocation| {
                allocation.generation_id == expected_generation
                    && allocation.supabase == expected_supabase
            })
            .ok_or_else(|| anyhow!("worktree was removed or replaced: {allocation_name:?}"))?;
        if let Some((owner, generation)) = expected_owner {
            state
                .allocations
                .get(&owner)
                .filter(|allocation| {
                    allocation.generation_id == generation
                        && matches!(allocation.supabase, SupabaseAllocation::Owned { .. })
                })
                .ok_or_else(|| {
                    anyhow!("shared Supabase owner was removed or replaced: {owner:?}")
                })?;
        }
        Ok(AllocationReadGuard {
            _selected_lock: selected_lock,
            _owner_lock: owner_lock,
            state,
        })
    }

    fn lock_allocation_with(
        &self,
        allocation_name: &str,
        mode: AllocationLockMode,
    ) -> Result<AllocationLock> {
        if crate::worktree::slug(allocation_name) != allocation_name {
            bail!("invalid allocation lock name {allocation_name:?}");
        }
        let directory = self
            .common_dir
            .join(STATE_DIR_NAME)
            .join(LIFECYCLE_LOCK_DIR_NAME);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join(format!("{allocation_name}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match mode {
            AllocationLockMode::Shared => FileExt::lock_shared(&file),
            AllocationLockMode::Exclusive => FileExt::lock_exclusive(&file),
        }
        .with_context(|| format!("lock {}", path.display()))?;
        Ok(AllocationLock { _file: file })
    }

    fn load_locked(&self) -> Result<(State, bool)> {
        let path = file_path(&self.common_dir);
        let input = match fs::read(&path) {
            Ok(input) => input,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((State::empty(), false));
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&input).with_context(|| format!("parse {}", path.display()))?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow!("{} has no integer state version", path.display()))?;

        match version {
            4 => {
                let mut state: State = serde_json::from_value(value)
                    .with_context(|| format!("parse state version 4 from {}", path.display()))?;
                let repaired = repair_legacy_shared_owners(&mut state)?;
                validate_state(&state)?;
                Ok((state, repaired))
            }
            3 => {
                let state: StateV3 = serde_json::from_value(value)
                    .with_context(|| format!("parse state version 3 from {}", path.display()))?;
                let state = self.migrate_v3(state)?;
                Ok((state, true))
            }
            version => bail!(
                "unsupported wrt state version {version}; expected {CURRENT_VER}; recreate the managed root with `wrt clone` or `wrt root init`"
            ),
        }
    }

    fn migrate_v3(&self, old: StateV3) -> Result<State> {
        if old.version != 3 {
            bail!("expected state version 3 during migration");
        }

        let primary_by_path = old.root.as_ref().and_then(|root| {
            let primary = Path::new(&root.main_worktree);
            let matches = old
                .allocations
                .iter()
                .filter(|(_, allocation)| paths_match(Path::new(&allocation.path), primary))
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [key] => Some(*key),
                _ => None,
            }
        });
        let block_zero = old
            .allocations
            .iter()
            .filter(|(_, allocation)| allocation.block == 0)
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>();
        let primary_key = primary_by_path.or(match block_zero.as_slice() {
            [key] => Some(*key),
            _ => None,
        });
        if let Some(primary_key) = primary_key {
            let primary = &old.allocations[primary_key];
            if !Path::new(&primary.path).exists() {
                bail!(
                    "cannot migrate state version 3 because the primary worktree {} is missing; repair it or recreate the managed root",
                    primary.path
                );
            }
        }
        let mut allocations = BTreeMap::new();
        for (key, allocation) in old.allocations {
            let checkout = Path::new(&allocation.path);
            if !checkout.exists() {
                continue;
            }
            let config = ProjectConfig::load_for(&self.config_root, checkout).map_err(|error| {
                anyhow!(
                    "cannot migrate live allocation {key:?} at {}: fix its invalid .wrt.json and retry: {error:#}",
                    checkout.display()
                )
            })?;
            let mut ports = match config {
                Some(config) => project_port_assignments(&config, allocation.offset),
                None => Ok(PortAssignments::new()),
            }
            .with_context(|| migration_recreate_message(&key))?;

            if let SupabaseAllocation::Owned { config_path, .. } = &allocation.supabase {
                let target = crate::supabase::Target::from_config_path(config_path)
                    .with_context(|| migration_recreate_message(&key))?;
                let claims = crate::supabase::port_claims(checkout, &target, 0)
                    .with_context(|| migration_recreate_message(&key))?;
                merge_port_assignments(&mut ports, claims)
                    .with_context(|| migration_recreate_message(&key))?;
            }

            allocations.insert(
                key,
                Allocation {
                    generation_id: AllocationGeneration::new(),
                    name: allocation.name,
                    branch: allocation.branch,
                    path: allocation.path,
                    block: allocation.block,
                    offset: allocation.offset,
                    ports,
                    status: allocation.status,
                    created_at: allocation.created_at,
                    supabase: allocation.supabase,
                    setup: AllocationSetup::default(),
                },
            );
        }

        let mut state = State {
            version: CURRENT_VER,
            root: old.root,
            allocations,
        };
        repair_legacy_shared_owners(&mut state)?;
        validate_state(&state).with_context(|| {
            "state version 3 cannot be migrated safely; recreate the conflicting worktree allocations"
        })?;
        Ok(state)
    }

    fn validate_external(&self, state: &State) -> Result<()> {
        let Some(root) = &state.root else {
            return Ok(());
        };
        if root.layout != LAYOUT_MANAGED_ROOT
            || !paths_match(Path::new(&root.managed_root), &self.managed_root)
            || !paths_match(Path::new(&root.git_common_dir), &self.common_dir)
            || !paths_match(Path::new(&root.main_worktree), &self.main_worktree)
            || !paths_match(Path::new(&root.worktrees_path), &self.worktree_parent)
        {
            bail!("state managed-root metadata does not match the detected Git repository");
        }

        let primary_count = state
            .allocations
            .values()
            .filter(|allocation| paths_match(Path::new(&allocation.path), &self.main_worktree))
            .count();
        let detached_without_primary = primary_count == 0
            && !self.main_worktree.exists()
            && state
                .allocations
                .values()
                .all(|allocation| allocation.block != 0);
        if primary_count != 1 && !detached_without_primary {
            bail!("state primary worktree must resolve to exactly one allocation");
        }

        let lexical_parent = lexical_absolute(&self.worktree_parent)?;
        let canonical_parent = fs::canonicalize(&self.worktree_parent).with_context(|| {
            format!("resolve worktrees path {}", self.worktree_parent.display())
        })?;
        for (key, allocation) in &state.allocations {
            let path = Path::new(&allocation.path);
            let lexical_path = lexical_absolute(path)?;
            if !lexical_path.starts_with(&lexical_parent) {
                bail!("allocation {key:?} path is outside worktreesPath");
            }
            match fs::canonicalize(path) {
                Ok(canonical) if !canonical.starts_with(&canonical_parent) => {
                    bail!("allocation {key:?} path resolves outside worktreesPath")
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("resolve allocation {key:?} path"));
                }
            }
            if let SupabaseAllocation::Shared { owner } = &allocation.supabase {
                let owner = state.allocations.get(owner).ok_or_else(|| {
                    anyhow!("allocation {key:?} references missing shared Supabase owner {owner:?}")
                })?;
                if !matches!(owner.supabase, SupabaseAllocation::Owned { .. })
                    || owner.generation_id == allocation.generation_id
                    || owner.path == allocation.path
                    || !Path::new(&owner.path).is_dir()
                {
                    bail!("allocation {key:?} has an invalid shared Supabase owner {owner:?}");
                }
            }
        }
        Ok(())
    }

    fn save_locked(&self, state: &State) -> Result<()> {
        let dir = state_dir(&self.common_dir);
        fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let mut bytes = serde_json::to_vec_pretty(state).context("format state JSON")?;
        bytes.push(b'\n');

        let mut temporary = tempfile::Builder::new()
            .prefix(".state-")
            .tempfile_in(&dir)
            .with_context(|| format!("create temporary state file in {}", dir.display()))?;
        temporary
            .as_file_mut()
            .write_all(&bytes)
            .with_context(|| format!("write temporary state file in {}", dir.display()))?;
        temporary
            .as_file()
            .sync_all()
            .with_context(|| format!("sync temporary state file in {}", dir.display()))?;
        let path = file_path(&self.common_dir);
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace {}", path.display()))?;
        sync_directory(&dir)?;
        Ok(())
    }
}

pub struct AllocationLock {
    _file: File,
}

pub struct AllocationReadGuard {
    _selected_lock: AllocationLock,
    _owner_lock: Option<AllocationLock>,
    state: State,
}

impl AllocationReadGuard {
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn allocation(&self, name: &str) -> &Allocation {
        &self.state.allocations[name]
    }
}

#[derive(Clone, Copy)]
enum AllocationLockMode {
    Shared,
    Exclusive,
}

impl State {
    pub fn empty() -> State {
        State {
            version: CURRENT_VER,
            root: None,
            allocations: BTreeMap::new(),
        }
    }

    pub fn sorted_allocations(&self) -> Vec<Allocation> {
        self.allocations.values().cloned().collect()
    }

    pub fn primary_allocation(&self) -> Option<(&str, &Allocation)> {
        let by_path = self.root.as_ref().and_then(|root| {
            let primary_path = Path::new(&root.main_worktree);
            self.allocations
                .iter()
                .find(|(_, allocation)| paths_match(Path::new(&allocation.path), primary_path))
        });

        by_path
            .or_else(|| {
                self.allocations
                    .iter()
                    .find(|(_, allocation)| allocation.block == 0)
            })
            .map(|(key, allocation)| (key.as_str(), allocation))
    }

    pub fn primary_allocation_key(&self) -> Option<&str> {
        self.primary_allocation().map(|(key, _)| key)
    }

    pub fn is_primary_allocation(&self, allocation: &Allocation) -> bool {
        self.primary_allocation().is_some_and(|(_, primary)| {
            paths_match(Path::new(&primary.path), Path::new(&allocation.path))
        })
    }

    pub fn allocation_mut_if_generation(
        &mut self,
        key: &str,
        expected: AllocationGeneration,
    ) -> Result<&mut Allocation> {
        let allocation = self
            .allocations
            .get_mut(key)
            .ok_or_else(|| anyhow!("allocation {key:?} no longer exists"))?;
        if allocation.generation_id != expected {
            bail!("allocation {key:?} was replaced while the operation was running");
        }
        Ok(allocation)
    }

    pub fn remove_if_generation(
        &mut self,
        key: &str,
        expected: AllocationGeneration,
    ) -> Result<Allocation> {
        self.allocation_mut_if_generation(key, expected)?;
        self.allocations
            .remove(key)
            .ok_or_else(|| anyhow!("allocation {key:?} no longer exists"))
    }
}

pub fn reserve_ports(state: &State, request: &ReservationRequest) -> Result<Reservation> {
    let base_ports = requested_base_ports(request)?;
    let used_blocks = state
        .allocations
        .values()
        .map(|allocation| allocation.block)
        .collect::<BTreeSet<_>>();
    let claimed_ports = state
        .allocations
        .values()
        .flat_map(|allocation| allocation.ports.values().copied())
        .collect::<BTreeSet<_>>();
    let stride = i32::from(request.port_stride);
    let max_block = base_ports
        .values()
        .max()
        .map(|max_base_port| (65535 - i32::from(*max_base_port)) / stride)
        .unwrap_or(i32::MAX / stride);
    if max_block < 1 {
        offset_port_assignments(&base_ports, stride)
            .with_context(|| "cannot reserve port block 1")?;
        return Err(anyhow!("no collision-free port blocks"));
    }

    for block in 1_i32..=max_block {
        if used_blocks.contains(&block) {
            continue;
        }
        let offset = block
            .checked_mul(stride)
            .ok_or_else(|| anyhow!("port offset overflow for block {block}"))?;
        let ports = offset_port_assignments(&base_ports, offset)
            .with_context(|| format!("cannot reserve port block {block}"))?;
        if ports.values().any(|port| claimed_ports.contains(port)) {
            continue;
        }
        return Ok(Reservation {
            block,
            offset,
            ports,
        });
    }

    Err(anyhow!("no collision-free port blocks"))
}

fn requested_base_ports(request: &ReservationRequest) -> Result<PortAssignments> {
    let mut ports = request.generic_ports.clone();
    for (key, port) in &request.isolated_supabase_ports {
        if ports.insert(key.clone(), *port).is_some() {
            bail!("duplicate requested port key {:?}", key.as_str());
        }
    }

    let mut claimed = BTreeMap::<u16, &PortKey>::new();
    for (key, port) in &ports {
        if *port == 0 {
            bail!("requested port {:?} must be in 1..=65535", key.as_str());
        }
        if let Some(other_key) = claimed.insert(*port, key) {
            bail!(
                "requested port {port} is duplicated by keys {:?} and {:?}",
                other_key.as_str(),
                key.as_str()
            );
        }
    }
    Ok(ports)
}

pub fn project_port_assignments(config: &ProjectConfig, offset: i32) -> Result<PortAssignments> {
    let base_ports = config
        .ports()
        .iter()
        .map(|spec| (spec.key().clone(), spec.base_port()))
        .collect();
    offset_port_assignments(&base_ports, offset)
}

fn offset_port_assignments(base_ports: &PortAssignments, offset: i32) -> Result<PortAssignments> {
    base_ports
        .iter()
        .map(|(key, base_port)| {
            let concrete = i32::from(*base_port)
                .checked_add(offset)
                .ok_or_else(|| anyhow!("port arithmetic overflow for {:?}", key.as_str()))?;
            if !(1..=65535).contains(&concrete) {
                bail!(
                    "port {} is out of range after offset: {base_port} -> {concrete}",
                    key.as_str()
                );
            }
            Ok((key.clone(), concrete as u16))
        })
        .collect()
}

pub fn merge_port_assignments(target: &mut PortAssignments, claims: PortAssignments) -> Result<()> {
    for (key, port) in claims {
        if target.insert(key.clone(), port).is_some() {
            bail!("duplicate port assignment key {:?}", key.as_str());
        }
    }
    validate_assignment_values(target)
}

fn validate_state(state: &State) -> Result<()> {
    if state.version != CURRENT_VER {
        bail!(
            "unsupported wrt state version {}; expected {CURRENT_VER}",
            state.version
        );
    }

    let mut claimed = BTreeMap::<u16, (&str, &PortKey)>::new();
    for (allocation_key, allocation) in &state.allocations {
        if allocation.name != *allocation_key
            || crate::worktree::slug(allocation_key) != *allocation_key
        {
            bail!("invalid allocation identity {allocation_key:?}");
        }
        for (field, mode) in [
            ("install", allocation.setup.install.as_str()),
            ("db", allocation.setup.db.as_str()),
        ] {
            if !matches!(mode, "auto" | "true" | "false") {
                bail!(
                    "invalid persisted setup {field} mode {mode:?} for allocation {allocation_key:?}"
                );
            }
        }
        validate_assignment_values(&allocation.ports)
            .map_err(|error| anyhow!("invalid ports for allocation {allocation_key:?}: {error}"))?;
        for (port_key, port) in &allocation.ports {
            if let Some((other_allocation, other_key)) =
                claimed.insert(*port, (allocation_key, port_key))
            {
                if other_allocation != allocation_key {
                    bail!(
                        "port {port} is claimed by allocation {other_allocation:?} key {:?} and allocation {allocation_key:?} key {:?}",
                        other_key.as_str(),
                        port_key.as_str()
                    );
                }
            }
        }
        if let SupabaseAllocation::Shared { owner } = &allocation.supabase {
            let owner_allocation = state.allocations.get(owner).ok_or_else(|| {
                anyhow!(
                    "allocation {allocation_key:?} references missing shared Supabase owner {owner:?}"
                )
            })?;
            if !matches!(owner_allocation.supabase, SupabaseAllocation::Owned { .. })
                || owner_allocation.path == allocation.path
                || owner_allocation.generation_id == allocation.generation_id
            {
                bail!("allocation {allocation_key:?} has invalid shared Supabase owner {owner:?}");
            }
        }
    }
    Ok(())
}

fn repair_legacy_shared_owners(state: &mut State) -> Result<bool> {
    let Some(primary_key) = state.primary_allocation_key().map(str::to_string) else {
        return Ok(false);
    };
    if !matches!(
        state
            .allocations
            .get(&primary_key)
            .map(|allocation| &allocation.supabase),
        Some(SupabaseAllocation::Owned { .. })
    ) {
        return Ok(false);
    }
    let legacy_main_is_not_owner = !matches!(
        state
            .allocations
            .get("main")
            .map(|allocation| &allocation.supabase),
        Some(SupabaseAllocation::Owned { .. })
    );
    let mut repaired = false;
    for allocation in state.allocations.values_mut() {
        if let SupabaseAllocation::Shared { owner } = &mut allocation.supabase
            && owner == "main"
            && legacy_main_is_not_owner
        {
            *owner = primary_key.clone();
            repaired = true;
        }
    }
    Ok(repaired)
}

fn validate_assignment_values(assignments: &PortAssignments) -> Result<()> {
    let mut claimed = BTreeMap::<u16, &PortKey>::new();
    for (key, port) in assignments {
        if !key.is_valid_state_key() {
            bail!("invalid persisted port key {:?}", key.as_str());
        }
        if *port == 0 {
            bail!("persisted port {:?} must be in 1..=65535", key.as_str());
        }
        if let Some(other_key) = claimed.insert(*port, key) {
            bail!(
                "port {port} is assigned to duplicate keys {:?} and {:?}",
                other_key.as_str(),
                key.as_str()
            );
        }
    }
    Ok(())
}

fn migration_recreate_message(allocation: &str) -> String {
    format!(
        "cannot migrate allocation {allocation:?} to state version 4; recreate the affected worktree"
    )
}

fn paths_match(first: &Path, second: &Path) -> bool {
    match (
        canonicalize_allow_missing(first),
        canonicalize_allow_missing(second),
    ) {
        (Ok(first), Ok(second)) => first == second,
        _ => lexical_absolute(first).ok() == lexical_absolute(second).ok(),
    }
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path).with_context(|| format!("resolve {}", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("path has no file name: {}", path.display()))?;
    Ok(fs::canonicalize(parent)
        .with_context(|| format!("resolve {}", parent.display()))?
        .join(name))
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("state path must be absolute: {}", path.display());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("state path escapes its root: {}", path.display());
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

struct StateLock {
    file: File,
}

impl StateLock {
    fn acquire(git_common_dir: &Path) -> Result<Self> {
        let dir = state_dir(git_common_dir);
        fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let path = dir.join(STATE_LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        FileExt::lock_exclusive(&file).with_context(|| format!("lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn sync_directory(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        match File::open(dir).and_then(|directory| directory.sync_all()) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
                ) => {}
            Err(error) => {
                return Err(error).with_context(|| format!("sync directory {}", dir.display()));
            }
        }
    }
    Ok(())
}

fn state_dir(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join(STATE_DIR_NAME)
}

fn file_path(git_common_dir: &Path) -> PathBuf {
    state_dir(git_common_dir).join(STATE_FILE_NAME)
}

#[derive(Deserialize)]
struct StateV3 {
    version: i32,
    #[serde(default)]
    root: Option<RootState>,
    #[serde(default)]
    allocations: BTreeMap<String, AllocationV3>,
}

#[derive(Deserialize)]
struct AllocationV3 {
    name: String,
    branch: String,
    path: String,
    block: i32,
    offset: i32,
    status: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(default)]
    supabase: SupabaseAllocation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    fn repo(root: &Path) -> Repo {
        Repo::new(
            root.to_path_buf(),
            root.join(".git"),
            root.join("main"),
            root.to_path_buf(),
            Some(root.join("main")),
        )
    }

    fn allocation(name: &str, block: i32) -> Allocation {
        Allocation {
            generation_id: AllocationGeneration::new(),
            name: name.to_string(),
            branch: name.to_string(),
            path: format!("/tmp/{name}"),
            block,
            offset: block * 100,
            ports: PortAssignments::new(),
            status: "active".to_string(),
            created_at: "x".to_string(),
            supabase: SupabaseAllocation::None,
            setup: AllocationSetup::default(),
        }
    }

    fn project_config(input: &str) -> ProjectConfig {
        let input = crate::project::complete_v2_fixture(input);
        ProjectConfig::from_slice(&input).unwrap()
    }

    #[test]
    fn reserve_reuses_block_holes_when_the_port_set_is_available() {
        let mut state = State::empty();
        state
            .allocations
            .insert("a".to_string(), allocation("a", 1));
        state
            .allocations
            .insert("b".to_string(), allocation("b", 3));
        let request = ReservationRequest::new(None, PortAssignments::new());

        assert_eq!(reserve_ports(&state, &request).unwrap().block, 2);
    }

    #[test]
    fn read_migrates_version_three_and_persists_concrete_ports() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        fs::create_dir_all(&repo.common_dir).unwrap();
        fs::create_dir_all(&repo.main_worktree).unwrap();
        fs::create_dir_all(temp.path().join("feature")).unwrap();
        fs::write(
            repo.config_root.join(".wrt.json"),
            crate::project::complete_v2_fixture(
                r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000}]}"#,
            ),
        )
        .unwrap();
        let state_dir = state_dir(&repo.common_dir);
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join(STATE_FILE_NAME),
            format!(
                r#"{{"version":3,"allocations":{{"main":{{"name":"main","branch":"main","path":{:?},"block":0,"offset":0,"status":"active","createdAt":"now"}},"feature":{{"name":"feature","branch":"feature","path":{:?},"block":2,"offset":200,"status":"active","createdAt":"now"}}}}}}"#,
                repo.main_worktree.to_string_lossy(),
                temp.path().join("feature").to_string_lossy()
            ),
        )
        .unwrap();

        let state = StateStore::new(&repo).read().unwrap();

        assert_eq!(state.version, 4);
        assert_eq!(
            state.allocations["main"].ports.values().copied().next(),
            Some(3000)
        );
        assert_eq!(
            state.allocations["feature"].ports.values().copied().next(),
            Some(3200)
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(state_dir.join(STATE_FILE_NAME)).unwrap()).unwrap();
        assert_eq!(saved["version"], 4);
        assert_eq!(saved["allocations"]["feature"]["ports"]["api"], 3200);
        for key in ["main", "feature"] {
            let generation_id = saved["allocations"][key]["generationId"].as_str().unwrap();
            assert!(Uuid::parse_str(generation_id).is_ok(), "{generation_id}");
        }
        assert_ne!(
            saved["allocations"]["main"]["generationId"],
            saved["allocations"]["feature"]["generationId"]
        );

        fs::write(
            repo.config_root.join(".wrt.json"),
            crate::project::complete_v2_fixture(
                r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":4000}]}"#,
            ),
        )
        .unwrap();
        let reloaded = StateStore::new(&repo).read().unwrap();
        assert_eq!(
            reloaded.allocations["feature"]
                .ports
                .values()
                .copied()
                .next(),
            Some(3200)
        );
    }

    #[test]
    fn migration_matches_equivalent_primary_path_spellings() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        fs::create_dir_all(&repo.main_worktree).unwrap();
        let spelled = repo.main_worktree.join("..").join("main");
        fs::write(
            file_path(&repo.common_dir),
            format!(
                r#"{{"version":3,"root":{{"layout":"managed-root","managedRoot":{:?},"gitCommonDir":{:?},"mainWorktree":{:?},"worktreesPath":{:?},"createdAt":"now"}},"allocations":{{"primary":{{"name":"primary","branch":"main","path":{:?},"block":7,"offset":700,"status":"active","createdAt":"now"}}}}}}"#,
                temp.path().to_string_lossy(),
                repo.common_dir.to_string_lossy(),
                repo.main_worktree.to_string_lossy(),
                temp.path().to_string_lossy(),
                spelled.to_string_lossy(),
            ),
        )
        .unwrap();

        let state = StateStore::new(&repo).read().unwrap();

        assert!(state.allocations.contains_key("primary"));
        assert_eq!(state.primary_allocation_key(), Some("primary"));
    }

    #[test]
    fn migration_repairs_legacy_main_owner_to_non_main_primary() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        let primary = &repo.main_worktree;
        let feature = temp.path().join("feature");
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        fs::create_dir_all(primary.join("supabase")).unwrap();
        fs::create_dir_all(&feature).unwrap();
        fs::write(
            primary.join("supabase/config.toml"),
            "project_id = \"primary\"\n",
        )
        .unwrap();
        fs::write(
            file_path(&repo.common_dir),
            format!(
                r#"{{"version":3,"root":{{"layout":"managed-root","managedRoot":{:?},"gitCommonDir":{:?},"mainWorktree":{:?},"worktreesPath":{:?},"createdAt":"now"}},"allocations":{{"staging":{{"name":"staging","branch":"main","path":{:?},"block":0,"offset":0,"status":"active","createdAt":"now","supabase":{{"mode":"owned","projectId":"primary","configPath":"supabase/config.toml"}}}},"feature":{{"name":"feature","branch":"feature","path":{:?},"block":1,"offset":100,"status":"active","createdAt":"now","supabase":{{"mode":"shared","owner":"main"}}}}}}}}"#,
                temp.path().to_string_lossy(),
                repo.common_dir.to_string_lossy(),
                primary.to_string_lossy(),
                temp.path().to_string_lossy(),
                primary.to_string_lossy(),
                feature.to_string_lossy(),
            ),
        )
        .unwrap();

        let state = StateStore::new(&repo).read().unwrap();

        assert_eq!(state.primary_allocation_key(), Some("staging"));
        assert_eq!(
            state.allocations["feature"].supabase,
            SupabaseAllocation::Shared {
                owner: "staging".to_string()
            }
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(file_path(&repo.common_dir)).unwrap()).unwrap();
        assert_eq!(
            saved["allocations"]["feature"]["supabase"]["owner"],
            "staging"
        );
    }

    #[test]
    fn compose_probe_allows_large_offsets_when_there_are_no_supabase_claims() {
        let config = project_config(
            r#"{"version":2,"port_stride":1,"ports":[{"key":"api","base_port":3000}]}"#,
        );
        let mut allocation = allocation("feature", 70000);
        allocation.offset = 70000;

        let request = ReservationRequest::compose_probe(&config, &allocation).unwrap();
        let reservation = reserve_ports(&State::empty(), &request).unwrap();

        assert_eq!(reservation.ports.values().next(), Some(&3001));
    }

    #[test]
    fn migration_keeps_owned_supabase_config_ports_concrete() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        let checkout = temp.path().join("feature");
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        fs::create_dir_all(checkout.join("supabase")).unwrap();
        fs::write(
            checkout.join("supabase/config.toml"),
            "project_id = \"project-feature\"\n[api]\nport = 54421\n",
        )
        .unwrap();
        fs::write(
            file_path(&repo.common_dir),
            format!(
                r#"{{"version":3,"allocations":{{"feature":{{"name":"feature","branch":"feature","path":{:?},"block":1,"offset":100,"status":"active","createdAt":"now","supabase":{{"mode":"owned","projectId":"project-feature","configPath":"supabase/config.toml"}}}}}}}}"#,
                checkout.to_string_lossy()
            ),
        )
        .unwrap();

        let state = StateStore::new(&repo).read().unwrap();
        let claim = state.allocations["feature"]
            .ports
            .iter()
            .find(|(key, _)| key.as_str() == "supabase.api.port")
            .map(|(_, port)| *port);

        assert_eq!(claim, Some(54421));
    }

    #[test]
    fn migration_discards_allocations_with_missing_checkouts() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        fs::write(
            repo.config_root.join(".wrt.json"),
            crate::project::complete_v2_fixture(
                r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":65500}]}"#,
            ),
        )
        .unwrap();
        fs::write(
            file_path(&repo.common_dir),
            r#"{"version":3,"allocations":{"feature":{"name":"feature","branch":"feature","path":"/missing/feature","block":1,"offset":100,"status":"active","createdAt":"now"}}}"#,
        )
        .unwrap();

        let state = StateStore::new(&repo).read().unwrap();

        assert!(state.allocations.is_empty());
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(file_path(&repo.common_dir)).unwrap()).unwrap();
        assert_eq!(saved["allocations"], serde_json::json!({}));
    }

    #[test]
    fn migration_refuses_to_discard_a_missing_primary_and_does_not_persist() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        let original = format!(
            r#"{{"version":3,"root":{{"layout":"managed-root","managedRoot":{:?},"gitCommonDir":{:?},"mainWorktree":{:?},"worktreesPath":{:?},"createdAt":"now"}},"allocations":{{"main":{{"name":"main","branch":"main","path":{:?},"block":0,"offset":0,"status":"active","createdAt":"now"}}}}}}"#,
            temp.path().to_string_lossy(),
            repo.common_dir.to_string_lossy(),
            repo.main_worktree.to_string_lossy(),
            temp.path().to_string_lossy(),
            repo.main_worktree.to_string_lossy(),
        );
        fs::write(file_path(&repo.common_dir), &original).unwrap();

        let error = StateStore::new(&repo).read().unwrap_err().to_string();
        assert!(error.contains("primary worktree"), "{error}");
        assert!(error.contains("repair it or recreate"), "{error}");
        assert_eq!(
            fs::read_to_string(file_path(&repo.common_dir)).unwrap(),
            original
        );
    }

    #[test]
    fn migration_uses_unique_block_zero_as_primary_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        let original = format!(
            r#"{{"version":3,"root":{{"layout":"managed-root","managedRoot":{:?},"gitCommonDir":{:?},"mainWorktree":{:?},"worktreesPath":{:?},"createdAt":"now"}},"allocations":{{"primary":{{"name":"primary","branch":"main","path":{:?},"block":0,"offset":0,"status":"active","createdAt":"now"}}}}}}"#,
            temp.path().to_string_lossy(),
            repo.common_dir.to_string_lossy(),
            repo.main_worktree.to_string_lossy(),
            temp.path().to_string_lossy(),
            temp.path().join("missing-primary").to_string_lossy(),
        );
        fs::write(file_path(&repo.common_dir), &original).unwrap();

        let error = StateStore::new(&repo).read().unwrap_err().to_string();

        assert!(error.contains("primary worktree"), "{error}");
        assert_eq!(
            fs::read_to_string(file_path(&repo.common_dir)).unwrap(),
            original
        );
    }

    #[test]
    fn migration_reports_invalid_live_config_and_can_retry_after_it_is_fixed() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        let checkout = temp.path().join("feature");
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join(".wrt.json"), r#"{"version":2,"wat":true}"#).unwrap();
        fs::write(
            file_path(&repo.common_dir),
            format!(
                r#"{{"version":3,"allocations":{{"feature":{{"name":"feature","branch":"feature","path":{:?},"block":1,"offset":100,"status":"active","createdAt":"now"}}}}}}"#,
                checkout.to_string_lossy()
            ),
        )
        .unwrap();

        let error = StateStore::new(&repo).read().unwrap_err().to_string();
        assert!(
            error.contains("cannot migrate live allocation \"feature\""),
            "{error}"
        );
        assert!(
            error.contains("fix its invalid .wrt.json and retry"),
            "{error}"
        );
        assert!(error.contains(&checkout.display().to_string()), "{error}");

        fs::write(
            checkout.join(".wrt.json"),
            crate::project::complete_v2_fixture(
                r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000}]}"#,
            ),
        )
        .unwrap();
        let recovered = StateStore::new(&repo).read().unwrap();
        assert_eq!(
            recovered.allocations["feature"].ports.values().next(),
            Some(&3100)
        );
    }

    #[test]
    fn concurrent_updates_reload_before_reserving_a_port_set() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        fs::create_dir_all(&repo.common_dir).unwrap();
        let store = StateStore::new(&repo);
        store.update(|_| Ok(())).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000},{"key":"postgres","base_port":5432}]}"#,
        );
        let request = ReservationRequest::new(Some(&config), PortAssignments::new());

        let handles = ["one", "two"].map(|name| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            let request = request.clone();
            thread::spawn(move || {
                barrier.wait();
                store
                    .update(|state| {
                        let reservation = reserve_ports(state, &request)?;
                        let mut allocated = allocation(name, reservation.block);
                        allocated.offset = reservation.offset;
                        allocated.ports = reservation.ports.clone();
                        state.allocations.insert(name.to_string(), allocated);
                        Ok(reservation)
                    })
                    .unwrap()
            })
        });
        barrier.wait();
        let reservations = handles.map(|handle| handle.join().unwrap());

        assert_ne!(reservations[0].block, reservations[1].block);
        assert!(
            reservations[0]
                .ports
                .values()
                .all(|port| !reservations[1].ports.values().any(|other| other == port))
        );
        let state = store.read().unwrap();
        assert_eq!(state.allocations.len(), 2);
    }

    #[test]
    fn allocation_lifecycle_lock_serializes_setup_and_remove() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = StateStore::new(&repo(temp.path()));
        let held = store.lock_allocation("feature").unwrap();
        let (sender, receiver) = mpsc::channel();
        let waiting_store = store.clone();
        let waiter = thread::spawn(move || {
            let _lock = waiting_store.lock_allocation("feature").unwrap();
            sender.send(()).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn shared_lifecycle_locks_allow_runtime_commands_and_block_cleanup() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = StateStore::new(&repo(temp.path()));
        let first = store.lock_allocation_shared("feature").unwrap();
        let (shared_sender, shared_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let shared_store = store.clone();
        let shared = thread::spawn(move || {
            let _lock = shared_store.lock_allocation_shared("feature").unwrap();
            shared_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        });
        shared_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (exclusive_sender, exclusive_receiver) = mpsc::channel();
        let exclusive_store = store.clone();
        let exclusive = thread::spawn(move || {
            let _lock = exclusive_store.lock_allocation("feature").unwrap();
            exclusive_sender.send(()).unwrap();
        });
        assert!(
            exclusive_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        drop(first);
        assert!(
            exclusive_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        release_sender.send(()).unwrap();
        exclusive_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        shared.join().unwrap();
        exclusive.join().unwrap();
    }

    #[test]
    fn allocation_read_guard_holds_selected_and_shared_owner_locks() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = StateStore::new(&repo(temp.path()));
        store
            .update(|state| {
                let mut owner = allocation("owner", 0);
                owner.supabase = SupabaseAllocation::Owned {
                    project_id: "owner-project".to_string(),
                    config_path: "supabase/config.toml".to_string(),
                };
                let mut client = allocation("client", 1);
                client.supabase = SupabaseAllocation::Shared {
                    owner: "owner".to_string(),
                };
                state.allocations.insert("owner".to_string(), owner);
                state.allocations.insert("client".to_string(), client);
                Ok(())
            })
            .unwrap();
        let snapshot = store.read().unwrap();
        let guard = store.lock_allocation_read(&snapshot, "client").unwrap();
        let (sender, receiver) = mpsc::channel();
        let waiting_store = store.clone();
        let waiter = thread::spawn(move || {
            let _lock = waiting_store.lock_allocation("owner").unwrap();
            sender.send(()).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(guard);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn allocation_read_guard_rechecks_selected_and_owner_generations() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = StateStore::new(&repo(temp.path()));
        store
            .update(|state| {
                let mut owner = allocation("owner", 0);
                owner.supabase = SupabaseAllocation::Owned {
                    project_id: "owner-project".to_string(),
                    config_path: "supabase/config.toml".to_string(),
                };
                let mut client = allocation("client", 1);
                client.supabase = SupabaseAllocation::Shared {
                    owner: "owner".to_string(),
                };
                state.allocations.insert("owner".to_string(), owner);
                state.allocations.insert("client".to_string(), client);
                Ok(())
            })
            .unwrap();
        let stale_client = store.read().unwrap();
        store
            .update(|state| {
                state.allocations.get_mut("client").unwrap().generation_id =
                    AllocationGeneration::new();
                Ok(())
            })
            .unwrap();
        assert!(store.lock_allocation_read(&stale_client, "client").is_err());

        let stale_owner = store.read().unwrap();
        store
            .update(|state| {
                state.allocations.get_mut("owner").unwrap().generation_id =
                    AllocationGeneration::new();
                Ok(())
            })
            .unwrap();
        assert!(store.lock_allocation_read(&stale_owner, "client").is_err());
    }

    #[test]
    fn state_rejects_mismatched_or_path_like_allocation_identity() {
        let mut state = State::empty();
        let mut mismatched = allocation("feature", 1);
        mismatched.name = "other".to_string();
        state.allocations.insert("feature".to_string(), mismatched);
        assert!(validate_state(&state).is_err());

        let mut state = State::empty();
        state
            .allocations
            .insert("../escape".to_string(), allocation("../escape", 1));
        assert!(validate_state(&state).is_err());
    }

    #[test]
    fn reserve_skips_block_60_when_eln_core_port_hits_main_minio() {
        let config = project_config(
            r#"{
              "version": 2,
              "port_stride": 100,
              "ports": [
                {"key":"postgres","base_port":5432},
                {"key":"minio","base_port":9000},
                {"key":"minio-console","base_port":9001},
                {"key":"rabbitmq","base_port":5672},
                {"key":"rabbitmq-admin","base_port":15672},
                {"key":"redis","base_port":6379},
                {"key":"core-api","base_port":3000},
                {"key":"notification","base_port":9024},
                {"key":"frontend","base_port":5173},
                {"key":"auth-api","base_port":3001},
                {"key":"auth-frontend","base_port":3002},
                {"key":"email","base_port":3003},
                {"key":"oauth","base_port":8181},
                {"key":"admin","base_port":3004}
              ]
            }"#,
        );
        let mut state = State::empty();
        let mut main = allocation("main", 0);
        main.offset = 0;
        main.ports = project_port_assignments(&config, 0).unwrap();
        state.allocations.insert("main".to_string(), main);
        for block in 1..60 {
            let name = format!("occupied-{block}");
            state
                .allocations
                .insert(name.clone(), allocation(&name, block));
        }
        let request = ReservationRequest::new(Some(&config), PortAssignments::new());

        let reservation = reserve_ports(&state, &request).unwrap();

        assert_eq!(reservation.block, 61);
        assert_eq!(reservation.offset, 6100);
        let core = config
            .ports()
            .iter()
            .find(|spec| spec.key().as_str() == "core-api")
            .unwrap();
        assert_eq!(reservation.ports[core.key()], 9100);
    }

    #[test]
    fn reserve_has_no_arbitrary_block_ceiling_below_the_port_range() {
        let config = project_config(
            r#"{"version":2,"port_stride":1,"ports":[{"key":"api","base_port":3000}]}"#,
        );
        let mut state = State::empty();
        for block in 1..10000 {
            let name = format!("occupied-{block}");
            state
                .allocations
                .insert(name.clone(), allocation(&name, block));
        }
        let request = ReservationRequest::new(Some(&config), PortAssignments::new());

        let reservation = reserve_ports(&state, &request).unwrap();

        assert_eq!(reservation.block, 10000);
        assert_eq!(reservation.offset, 10000);
        assert_eq!(reservation.ports.values().next(), Some(&13000));
    }

    #[test]
    fn reserve_offsets_isolated_supabase_ports_in_the_same_collision_set() {
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000}]}"#,
        );
        let supabase_key = PortKey::supabase_claim(&["api".to_string(), "port".to_string()]);
        let mut supabase_ports = PortAssignments::new();
        supabase_ports.insert(supabase_key.clone(), 54321);
        let mut state = State::empty();
        let occupied_config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"occupied","base_port":54421}]}"#,
        );
        let mut main = allocation("main", 0);
        main.ports = project_port_assignments(&occupied_config, 0).unwrap();
        state.allocations.insert("main".to_string(), main);
        let request = ReservationRequest::new(Some(&config), supabase_ports);

        let reservation = reserve_ports(&state, &request).unwrap();

        assert_eq!(reservation.block, 2);
        assert_eq!(reservation.ports[&supabase_key], 54521);
        assert_eq!(
            reservation
                .ports
                .values()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
    }

    #[test]
    fn reserve_rejects_duplicate_requested_ports() {
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000},{"key":"admin","base_port":3000}]}"#,
        );
        let request = ReservationRequest::new(Some(&config), PortAssignments::new());

        let error = reserve_ports(&State::empty(), &request)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("requested port 3000 is duplicated"),
            "{error}"
        );
    }

    #[test]
    fn reserve_rejects_candidate_ports_outside_the_valid_range() {
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":65500}]}"#,
        );
        let request = ReservationRequest::new(Some(&config), PortAssignments::new());

        let error = format!(
            "{:#}",
            reserve_ports(&State::empty(), &request).unwrap_err()
        );

        assert!(error.contains("cannot reserve port block 1"), "{error}");
        assert!(error.contains("out of range"), "{error}");
    }

    #[test]
    fn generic_and_owned_supabase_ports_share_collision_validation() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        let feature = temp.path().join("feature");
        fs::create_dir_all(repo.common_dir.join(STATE_DIR_NAME)).unwrap();
        fs::create_dir_all(feature.join("supabase")).unwrap();
        fs::write(
            feature.join("supabase/config.toml"),
            "project_id = \"feature\"\n[api]\nport = 54321\n",
        )
        .unwrap();
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":54321}]}"#,
        );
        let generic = project_port_assignments(&config, 0).unwrap();
        let target = crate::supabase::Target::from_config_path("supabase/config.toml").unwrap();
        let supabase = crate::supabase::port_claims(&feature, &target, 0).unwrap();
        let store = StateStore::new(&repo);

        let error = store
            .update(|state| {
                let mut main = allocation("main", 0);
                main.ports = generic;
                let mut feature = allocation("feature", 1);
                feature.ports = supabase;
                feature.supabase = SupabaseAllocation::Owned {
                    project_id: "feature".to_string(),
                    config_path: "supabase/config.toml".to_string(),
                };
                state.allocations.insert("main".to_string(), main);
                state.allocations.insert("feature".to_string(), feature);
                Ok(())
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("port 54321 is claimed"), "{error}");
    }

    #[test]
    fn generic_and_supabase_ports_cannot_collide_within_one_allocation() {
        let temp = tempfile::TempDir::new().unwrap();
        let feature = temp.path().join("feature");
        fs::create_dir_all(feature.join("supabase")).unwrap();
        fs::write(
            feature.join("supabase/config.toml"),
            "project_id = \"feature\"\n[api]\nport = 54321\n",
        )
        .unwrap();
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":54321}]}"#,
        );
        let mut assignments = project_port_assignments(&config, 0).unwrap();
        let target = crate::supabase::Target::from_config_path("supabase/config.toml").unwrap();
        let claims = crate::supabase::port_claims(&feature, &target, 0).unwrap();

        let error = merge_port_assignments(&mut assignments, claims)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("port 54321 is assigned to duplicate keys"),
            "{error}"
        );
        assert!(error.contains("api"), "{error}");
        assert!(error.contains("supabase.api.port"), "{error}");
    }

    #[test]
    fn persisted_allocation_rejects_duplicate_concrete_ports() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = repo(temp.path());
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000},{"key":"admin","base_port":3000}]}"#,
        );
        let ports = project_port_assignments(&config, 0).unwrap();

        let error = StateStore::new(&repo)
            .update(|state| {
                let mut allocation = allocation("feature", 1);
                allocation.ports = ports;
                state.allocations.insert("feature".to_string(), allocation);
                Ok(())
            })
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("port 3000 is assigned to duplicate keys"),
            "{error}"
        );
    }

    #[test]
    fn stale_generation_cannot_mutate_or_remove_recreated_allocation() {
        let mut state = State::empty();
        let original = allocation("feature", 1);
        let stale_generation = original.generation_id;
        state.allocations.insert("feature".to_string(), original);
        let replacement = allocation("feature", 2);
        let replacement_generation = replacement.generation_id;
        state.allocations.insert("feature".to_string(), replacement);

        let mutate_error = state
            .allocation_mut_if_generation("feature", stale_generation)
            .unwrap_err()
            .to_string();
        let remove_error = state
            .remove_if_generation("feature", stale_generation)
            .unwrap_err()
            .to_string();

        assert!(mutate_error.contains("was replaced"), "{mutate_error}");
        assert!(remove_error.contains("was replaced"), "{remove_error}");
        assert_eq!(
            state.allocations["feature"].generation_id,
            replacement_generation
        );
    }

    #[test]
    fn port_assignment_reports_checked_arithmetic_overflow() {
        let config = project_config(
            r#"{"version":2,"port_stride":100,"ports":[{"key":"api","base_port":3000}]}"#,
        );

        let error = project_port_assignments(&config, i32::MAX)
            .unwrap_err()
            .to_string();

        assert!(error.contains("port arithmetic overflow"), "{error}");
    }

    #[test]
    fn primary_allocation_follows_managed_root_path_instead_of_main_key() {
        let mut state = State::empty();
        state.root = Some(RootState {
            layout: LAYOUT_MANAGED_ROOT.to_string(),
            managed_root: "/repo".to_string(),
            git_common_dir: "/repo/.git".to_string(),
            main_worktree: "/repo/staging".to_string(),
            worktrees_path: "/repo".to_string(),
            created_at: "now".to_string(),
            supabase_config_path: None,
        });
        state
            .allocations
            .insert("main".to_string(), allocation("main", 2));
        state
            .allocations
            .insert("staging".to_string(), allocation("staging", 0));
        state.allocations.get_mut("main").unwrap().path = "/repo/main".to_string();
        state.allocations.get_mut("staging").unwrap().path = "/repo/staging".to_string();

        assert_eq!(state.primary_allocation_key(), Some("staging"));
        assert!(state.is_primary_allocation(&state.allocations["staging"]));
        assert!(!state.is_primary_allocation(&state.allocations["main"]));
    }
}
