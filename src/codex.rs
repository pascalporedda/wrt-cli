use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

#[derive(Clone, Debug)]
pub struct DiscoverOpts {
    pub repo_root: PathBuf,
    pub model: String,
}

impl Default for DiscoverOpts {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::new(),
            model: DEFAULT_MODEL.to_string(),
        }
    }
}

static SCHEMA_BYTES: &[u8] = include_bytes!("../assets/wrt-discovery.schema.json");
static PROMPT_TEXT: &str = include_str!("../assets/discover.txt");

pub const DEFAULT_MODEL: &str = "gpt-5.6-sol";
const REASONING_EFFORT: &str = "medium";

pub fn discover(opts: DiscoverOpts) -> Result<Vec<u8>> {
    if let Ok(v) = std::env::var("WRT_CODEX_MOCK_OUTPUT") {
        if !v.trim().is_empty() {
            let b = fs::read(&v).with_context(|| format!("read {v}"))?;
            return Ok(b);
        }
    }

    let codex = which("codex")?;

    let tmp = TempDir::new().context("mk temp dir")?;
    let schema_path = tmp.path().join("schema.json");
    let out_path = tmp.path().join("out.json");
    fs::write(&schema_path, SCHEMA_BYTES)
        .with_context(|| format!("write {}", schema_path.display()))?;

    let status = Command::new(codex)
        .arg("exec")
        .arg(PROMPT_TEXT)
        .arg("--output-schema")
        .arg(&schema_path)
        .arg("-o")
        .arg(&out_path)
        .arg("--model")
        .arg(&opts.model)
        .arg("-c")
        .arg(format!("model_reasoning_effort={REASONING_EFFORT}"))
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
    Ok(b)
}

fn which(bin: &str) -> Result<PathBuf> {
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
    use crate::project::ProjectConfig;

    const DISCOVERY_V2: &str = r#"{
      "version": 2,
      "port_stride": 100,
      "ports": [
        {
          "key": "postgres",
          "base_port": 5432,
          "outputs": [
            {"env": "POSTGRES_PORT", "template": "{port}"},
            {"env": "DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/core"},
            {"env": "AUTH_DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/auth"},
            {"env": "NOTIFICATION_DATABASE_URL", "template": "postgresql://postgres:postgres@localhost:{port}/notification"}
          ]
        },
        {
          "key": "redis",
          "base_port": 6379,
          "outputs": [{"env": "REDIS_PORT", "template": "{port}"}]
        },
        {
          "key": "core-api",
          "base_port": 3000,
          "outputs": [
            {"env": "CORE_API_PORT", "template": "{port}"},
            {"env": "VITE_API_URL", "template": "http://localhost:{port}/api/v1"}
          ]
        },
        {
          "key": "web",
          "base_port": 5173,
          "outputs": [{"env": "WEB_PORT", "template": "{port}"}]
        }
      ],
      "commands": {
        "setup": {"argv": ["pnpm", "setup"], "cwd": null},
        "start": {"argv": ["pnpm", "start"], "cwd": null},
        "stop": {"argv": ["pnpm", "stop"], "cwd": null},
        "status": {"argv": ["pnpm", "status"], "cwd": null},
        "db_migrate": {"argv": ["pnpm", "db:migrate"], "cwd": null},
        "db_seed": {"argv": ["pnpm", "db:seed"], "cwd": null},
        "db_reset": {"argv": ["pnpm", "db:reset"], "cwd": null}
      },
      "compose": {"files": ["compose.yaml"]},
      "supabase": null
    }"#;

    fn must_be_object(v: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
        v.as_object().expect("object")
    }

    fn check_required_includes_all_properties(schema: &serde_json::Value) {
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&serde_json::Value::Bool(false)),
                "object schema must reject unknown fields"
            );
            let required = schema
                .get("required")
                .and_then(|v| v.as_array())
                .expect("object schema with properties must have required array");

            for k in props.keys() {
                let ok = required.iter().any(|x| x.as_str() == Some(k.as_str()));
                assert!(ok, "required missing property key: {k}");
            }

            for v in props.values() {
                check_required_includes_all_properties(v);
            }
        }

        if let Some(items) = schema.get("items") {
            check_required_includes_all_properties(items);
        }
    }

    #[test]
    fn embedded_schema_meets_codex_required_rules() {
        let v: serde_json::Value = serde_json::from_slice(SCHEMA_BYTES).expect("schema json");
        check_required_includes_all_properties(&v);

        let root = must_be_object(&v);
        assert_eq!(root["properties"]["version"]["const"], 2);
        assert_eq!(
            property_names(&root["properties"]),
            [
                "commands",
                "compose",
                "port_stride",
                "ports",
                "supabase",
                "version",
            ]
        );
        assert_eq!(
            property_names(&root["properties"]["commands"]["properties"]),
            [
                "db_migrate",
                "db_reset",
                "db_seed",
                "setup",
                "start",
                "status",
                "stop",
            ]
        );
    }

    #[test]
    fn discovery_schema_and_project_config_accept_the_same_v2_shape() {
        let config = ProjectConfig::from_slice(DISCOVERY_V2.as_bytes()).unwrap();

        assert_eq!(config.port_stride(), 100);
        assert_eq!(config.ports().len(), 4);
        assert_eq!(config.commands().setup().unwrap().argv(), ["pnpm", "setup"]);
        assert!(config.supabase().is_none());
    }

    #[test]
    fn discovery_schema_nullable_fields_map_to_project_config_options() {
        let config = ProjectConfig::from_slice(
            br#"{
              "version": 2,
              "port_stride": 100,
              "ports": [],
              "commands": {
                "setup": null,
                "start": null,
                "stop": null,
                "status": null,
                "db_migrate": null,
                "db_seed": null,
                "db_reset": null
              },
              "compose": null,
              "supabase": null
            }"#,
        )
        .unwrap();

        assert!(config.ports().is_empty());
        assert!(config.commands().setup().is_none());
        assert!(config.supabase().is_none());
    }

    #[test]
    fn embedded_prompt_defines_the_project_owned_contract() {
        for text in [
            "Use `null`",
            "host-reachable port",
            "must be safe to run again",
            "Never use a reset command as `setup`",
            "db_migrate",
            "db_seed",
            "db_reset",
            "ordered repository-relative Compose files",
            "cannot use a host `localhost` URL",
            "supabase/config.toml",
        ] {
            assert!(PROMPT_TEXT.contains(text), "prompt missing {text:?}");
        }
    }

    fn property_names(value: &serde_json::Value) -> Vec<&str> {
        must_be_object(value).keys().map(String::as_str).collect()
    }
}
