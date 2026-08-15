use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const STATE_DIR_NAME: &str = ".wrt";
const STATE_FILE_NAME: &str = "state.json";
const CURRENT_VER: i32 = 3;

pub const LAYOUT_MANAGED_ROOT: &str = "managed-root";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
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
    pub name: String,
    pub branch: String,
    pub path: String,
    pub block: i32,
    pub offset: i32,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(default)]
    pub supabase: SupabaseAllocation,
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

impl State {
    pub fn empty() -> State {
        State {
            version: CURRENT_VER,
            root: None,
            allocations: BTreeMap::new(),
        }
    }

    pub fn load(git_common_dir: &Path) -> Result<State> {
        let p = file_path(git_common_dir);
        let b = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::empty()),
            Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
        };

        let st: State =
            serde_json::from_slice(&b).with_context(|| format!("parse {}", p.display()))?;
        if st.version != CURRENT_VER {
            return Err(anyhow!(
                "unsupported wrt state version {}; expected {CURRENT_VER}; recreate the managed root with `wrt clone` or `wrt root init`",
                st.version
            ));
        }
        Ok(st)
    }

    pub fn save(&self, git_common_dir: &Path) -> Result<()> {
        let dir = git_common_dir.join(STATE_DIR_NAME);
        fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let mut b = serde_json::to_vec_pretty(self).context("json format")?;
        b.push(b'\n');
        let p = file_path(git_common_dir);
        fs::write(&p, &b).with_context(|| format!("write {}", p.display()))?;
        Ok(())
    }

    pub fn allocate_block(&self) -> Result<i32> {
        let mut used: BTreeSet<i32> = BTreeSet::new();
        for a in self.allocations.values() {
            used.insert(a.block);
        }
        // Block 0 is reserved for the main workdir (default ports).
        for i in 1..10000 {
            if !used.contains(&i) {
                return Ok(i);
            }
        }
        Err(anyhow!("no free port blocks"))
    }

    pub fn sorted_allocations(&self) -> Vec<Allocation> {
        self.allocations.values().cloned().collect()
    }

    pub fn primary_allocation(&self) -> Option<(&str, &Allocation)> {
        let by_path = self.root.as_ref().and_then(|root| {
            let primary_path = Path::new(&root.main_worktree);
            self.allocations
                .iter()
                .find(|(_, allocation)| Path::new(&allocation.path) == primary_path)
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
        self.primary_allocation()
            .is_some_and(|(_, primary)| primary.path == allocation.path)
    }
}

fn file_path(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join(STATE_DIR_NAME).join(STATE_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_block_skips_0_and_reuses_holes() {
        let mut st = State { ..State::empty() };
        st.allocations.insert(
            "a".to_string(),
            Allocation {
                name: "a".to_string(),
                branch: "a".to_string(),
                path: "/tmp/a".to_string(),
                block: 1,
                offset: 100,
                status: "active".to_string(),
                created_at: "x".to_string(),
                supabase: SupabaseAllocation::None,
            },
        );
        st.allocations.insert(
            "b".to_string(),
            Allocation {
                name: "b".to_string(),
                branch: "b".to_string(),
                path: "/tmp/b".to_string(),
                block: 3,
                offset: 300,
                status: "active".to_string(),
                created_at: "x".to_string(),
                supabase: SupabaseAllocation::None,
            },
        );

        assert_eq!(st.allocate_block().unwrap(), 2);
    }

    #[test]
    fn load_rejects_version_two_state() {
        let td = tempfile::TempDir::new().unwrap();
        let state_dir = td.path().join(".wrt");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("state.json"),
            r#"{"version":2,"allocations":{"x":{"name":"x","branch":"x","path":"/tmp/x","block":1,"offset":100,"status":"active","createdAt":"now"}}}"#,
        )
        .unwrap();

        let error = State::load(td.path()).unwrap_err().to_string();
        assert!(error.contains("unsupported wrt state version"), "{error}");
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
        state.allocations.insert(
            "main".to_string(),
            Allocation {
                name: "main".to_string(),
                branch: "main".to_string(),
                path: "/repo/main".to_string(),
                block: 2,
                offset: 200,
                status: "active".to_string(),
                created_at: "later".to_string(),
                supabase: SupabaseAllocation::None,
            },
        );
        state.allocations.insert(
            "staging".to_string(),
            Allocation {
                name: "staging".to_string(),
                branch: "staging".to_string(),
                path: "/repo/staging".to_string(),
                block: 0,
                offset: 0,
                status: "active".to_string(),
                created_at: "now".to_string(),
                supabase: SupabaseAllocation::None,
            },
        );

        assert_eq!(state.primary_allocation_key(), Some("staging"));
        assert!(state.is_primary_allocation(&state.allocations["staging"]));
        assert!(!state.is_primary_allocation(&state.allocations["main"]));
    }
}
