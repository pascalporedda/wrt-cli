use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::codex;
use crate::ui;

pub fn cmd_init(
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
