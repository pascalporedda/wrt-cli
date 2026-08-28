use anyhow::Result;
use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::codex;
use crate::project::ProjectConfig;
use crate::ui;
use crate::util::{atomic_write_private, confirm, sh_quote};

pub fn cmd_init(
    log: &ui::Logger,
    discovery_root: &Path,
    output_root: &Path,
    force: bool,
    print_only: bool,
    accept_commands: bool,
    model: Option<String>,
) -> Result<i32> {
    let out_path = output_root.join(".wrt.json");
    let output_present = match std::fs::symlink_metadata(&out_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !print_only && !force && output_present {
        log.errorf(&format!(
            "{} already exists (use --force to overwrite)",
            out_path.display()
        ));
        return Ok(2);
    }

    log.infof("running codex discovery (writes .wrt.json config)");
    let mut opts = codex::DiscoverOpts {
        repo_root: discovery_root.to_path_buf(),
        ..Default::default()
    };
    if let Some(model) = model {
        opts.model = model;
    }
    let raw = match codex::discover(opts) {
        Ok(v) => v,
        Err(e) => {
            log.errorf(&format!("{e}"));
            log.errorf("hint: install/auth codex CLI, or set WRT_CODEX_MOCK_OUTPUT=/path/to/out.json for offline testing");
            return Ok(1);
        }
    };

    let config = match ProjectConfig::from_discovery_slice(&raw) {
        Ok(config) => config,
        Err(error) => {
            log.errorf(&format!("invalid project config from Codex: {error:#}"));
            return Ok(1);
        }
    };
    if let Err(error) = config.validate_discovery_paths(discovery_root) {
        log.errorf(&format!("invalid project config from Codex: {error:#}"));
        return Ok(1);
    }

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

    let commands = config.discovered_commands();
    if !commands.is_empty() && !accept_commands {
        for (name, command) in &commands {
            let argv = command
                .argv()
                .iter()
                .map(|argument| sh_quote(argument))
                .collect::<Vec<_>>()
                .join(" ");
            let cwd = command
                .cwd()
                .map_or_else(|| ".".to_string(), |path| path.display().to_string());
            log.infof(&format!("discovered command {name}: {argv} (cwd {cwd})"));
        }
        if !std::io::stdin().is_terminal() {
            log.errorf("discovery produced executable commands; inspect with --print, then pass --accept-commands to write them");
            return Ok(2);
        }
        if !confirm("Write this config and allow wrt to execute these commands? (y/N): ")? {
            log.infof("config not written");
            return Ok(0);
        }
    }

    atomic_write_private(output_root, &out_path, &pretty)?;
    log.infof(&format!("wrote {}", out_path.display()));
    Ok(0)
}
