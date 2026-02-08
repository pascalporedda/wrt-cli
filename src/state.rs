use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const STATE_DIR_NAME: &str = ".wrt";
const STATE_FILE_NAME: &str = "state.json";
const CURRENT_VER: i32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub version: i32,
    #[serde(default)]
    pub allocations: BTreeMap<String, Allocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allocation {
    pub name: String,
    pub branch: String,
    pub path: String,
    pub block: i32,
    pub offset: i32,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

impl State {
    pub fn load(git_common_dir: &Path) -> Result<State> {
        let p = file_path(git_common_dir);
        let b = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(State {
                    version: CURRENT_VER,
                    allocations: BTreeMap::new(),
                })
            }
            Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
        };

        let mut st: State =
            serde_json::from_slice(&b).with_context(|| format!("parse {}", p.display()))?;
        if st.version == 0 {
            st.version = CURRENT_VER;
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
}

fn file_path(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join(STATE_DIR_NAME).join(STATE_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_block_skips_0_and_reuses_holes() {
        let mut st = State {
            version: CURRENT_VER,
            allocations: BTreeMap::new(),
        };
        st.allocations.insert(
            "a".to_string(),
            Allocation {
                name: "a".to_string(),
                branch: "a".to_string(),
                path: "/tmp/a".to_string(),
                block: 1,
                offset: 100,
                created_at: "x".to_string(),
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
                created_at: "x".to_string(),
            },
        );

        assert_eq!(st.allocate_block().unwrap(), 2);
    }
}
