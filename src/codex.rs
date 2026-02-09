use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Discovery {
    pub version: i32,
    #[serde(rename = "port_block_size")]
    pub port_block_size: i32,

    pub package_manager: PackageManager,

    #[serde(default)]
    pub services: Vec<Service>,

    #[serde(default)]
    pub database: Database,

    pub supabase: Supabase,

    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PackageManager {
    pub name: String,
    pub install_command: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub dev_command: Vec<String>,
    #[serde(default)]
    pub base_port: Option<i32>,
    #[serde(default)]
    pub port_env: Option<String>,
    #[serde(default)]
    pub url_env: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Supabase {
    pub detected: bool,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub start_command: Option<Vec<String>>,
    #[serde(default)]
    pub base_ports: Option<BasePorts>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Database {
    pub detected: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub migrate_command: Option<Vec<String>>,
    #[serde(default)]
    pub seed_command: Option<Vec<String>>,
    #[serde(default)]
    pub reset_command: Option<Vec<String>>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BasePorts {
    #[serde(default)]
    pub api: Option<i32>,
    #[serde(default)]
    pub db: Option<i32>,
    #[serde(default, rename = "shadow_db")]
    pub shadow_db: Option<i32>,
    #[serde(default)]
    pub studio: Option<i32>,
    #[serde(default)]
    pub inbucket: Option<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct DiscoverOpts {
    pub repo_root: PathBuf,
    pub model: Option<String>,
}

static SCHEMA_BYTES: &[u8] = include_bytes!("../assets/wrt-discovery.schema.json");
static PROMPT_TEXT: &str = include_str!("../assets/discover.txt");

pub fn discover(opts: DiscoverOpts) -> Result<(Vec<u8>, Discovery)> {
    if let Ok(v) = std::env::var("WRT_CODEX_MOCK_OUTPUT") {
        if !v.trim().is_empty() {
            let b = fs::read(&v).with_context(|| format!("read {v}"))?;
            let d: Discovery = serde_json::from_slice(&b).unwrap_or_default();
            return Ok((b, d));
        }
    }

    // Fail early with a clear message if codex isn't installed.
    let codex = which("codex")?;

    let tmp = TempDir::new().context("mk temp dir")?;
    let schema_path = tmp.path().join("schema.json");
    let out_path = tmp.path().join("out.json");
    fs::write(&schema_path, SCHEMA_BYTES)
        .with_context(|| format!("write {}", schema_path.display()))?;

    let mut args: Vec<String> = vec![
        "exec".into(),
        PROMPT_TEXT.to_string(),
        "--output-schema".into(),
        schema_path.to_string_lossy().to_string(),
        "-o".into(),
        out_path.to_string_lossy().to_string(),
    ];
    if let Some(m) = opts
        .model
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        args.push("--model".into());
        args.push(m.to_string());
    }

    let status = Command::new(codex)
        .args(args)
        .current_dir(&opts.repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .context("run codex")?;

    if !status.success() {
        return Err(anyhow!("codex exec failed"));
    }

    let b = fs::read(&out_path).with_context(|| format!("read {}", out_path.display()))?;
    let d: Discovery = serde_json::from_slice(&b).unwrap_or_default();
    Ok((b, d))
}

fn which(bin: &str) -> Result<PathBuf> {
    // Minimal "which" to avoid pulling in more deps.
    let path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH not set"))?;
    for p in std::env::split_paths(&path) {
        let cand = p.join(bin);
        if cand.exists() {
            return Ok(cand);
        }
        #[cfg(windows)]
        {
            let cand = p.join(format!("{bin}.exe"));
            if cand.exists() {
                return Ok(cand);
            }
        }
    }
    Err(anyhow!("codex not found in PATH"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn must_be_object(v: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
        v.as_object().expect("object")
    }

    fn check_required_includes_all_properties(schema: &serde_json::Value) {
        // Codex/OpenAI response_format schema validation is stricter than general JSON Schema:
        // if an object defines properties, it must define required and include every property key.
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            let required = schema
                .get("required")
                .and_then(|v| v.as_array())
                .expect("object schema with properties must have required array");

            for k in props.keys() {
                let ok = required.iter().any(|x| x.as_str() == Some(k.as_str()));
                assert!(ok, "required missing property key: {k}");
            }

            // Recurse into property schemas.
            for v in props.values() {
                check_required_includes_all_properties(v);
            }
        }

        // Recurse into array item schemas.
        if let Some(items) = schema.get("items") {
            check_required_includes_all_properties(items);
        }
    }

    #[test]
    fn embedded_schema_meets_codex_required_rules() {
        let v: serde_json::Value = serde_json::from_slice(SCHEMA_BYTES).expect("schema json");
        check_required_includes_all_properties(&v);

        // Quick regression for the reported failure.
        let pm = must_be_object(&v)["properties"]["package_manager"].clone();
        let req = pm
            .get("required")
            .and_then(|v| v.as_array())
            .expect("package_manager.required");
        assert!(req.iter().any(|x| x.as_str() == Some("notes")));
    }

    #[test]
    fn embedded_prompt_mentions_null_for_unknown_fields() {
        assert!(PROMPT_TEXT.contains("use null"));
        assert!(PROMPT_TEXT.contains("Do not omit"));
    }
}
