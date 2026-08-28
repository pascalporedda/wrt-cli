use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

use crate::envx::ResolvedEnvironment;
use crate::gitx::Repo;
use crate::project::{ComposeSpec, ProjectConfig};
use crate::state::{Allocation, State};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposeReport {
    findings: Vec<ComposeFinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposeFinding {
    pub code: ComposeFindingCode,
    pub service: Option<String>,
    pub field: String,
    pub observed: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposeFindingCode {
    RenderFailed,
    MalformedOutput,
    UnsafeSyntheticPort,
    FixedHostPort,
    DuplicateHostPort,
    FixedContainerName,
    ChangedOutputShape,
}

impl fmt::Display for ComposeFindingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::RenderFailed => "render-failed",
            Self::MalformedOutput => "malformed-output",
            Self::UnsafeSyntheticPort => "unsafe-synthetic-port",
            Self::FixedHostPort => "fixed-host-port",
            Self::DuplicateHostPort => "duplicate-host-port",
            Self::FixedContainerName => "fixed-container-name",
            Self::ChangedOutputShape => "changed-output-shape",
        };
        formatter.write_str(value)
    }
}

impl ComposeReport {
    fn new(findings: Vec<ComposeFinding>) -> Self {
        Self { findings }
    }

    pub fn is_safe(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn findings(&self) -> &[ComposeFinding] {
        &self.findings
    }
}

pub fn inspect(
    repo: &Repo,
    state: &State,
    allocation: &Allocation,
    project: &ProjectConfig,
    first_environment: &ResolvedEnvironment,
) -> Result<Option<ComposeReport>> {
    let Some(spec) = project.compose() else {
        return Ok(None);
    };
    Ok(Some(inspect_with_renderer(
        repo,
        state,
        allocation,
        project,
        spec,
        first_environment,
        render_compose,
    )))
}

pub fn format_findings(report: &ComposeReport) -> String {
    report
        .findings()
        .iter()
        .map(|finding| {
            let service = finding
                .service
                .as_deref()
                .map(|service| format!(" service={service}"))
                .unwrap_or_default();
            let observed = finding
                .observed
                .iter()
                .map(|value| format!("{value:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}{} field={} observed=[{}]",
                finding.code, service, finding.field, observed
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn inspect_with_renderer<F>(
    repo: &Repo,
    state: &State,
    allocation: &Allocation,
    project: &ProjectConfig,
    spec: &ComposeSpec,
    first_environment: &ResolvedEnvironment,
    mut render: F,
) -> ComposeReport
where
    F: FnMut(&Path, &ComposeSpec, &ResolvedEnvironment) -> Result<Vec<u8>>,
{
    let mut findings = Vec::new();
    let second_allocation = match synthetic_allocation(state, allocation, project) {
        Ok(allocation) => allocation,
        Err(error) => {
            findings.push(ComposeFinding {
                code: ComposeFindingCode::UnsafeSyntheticPort,
                service: None,
                field: "ports".to_string(),
                observed: vec![error.to_string()],
            });
            return ComposeReport::new(findings);
        }
    };
    let second_environment = match ResolvedEnvironment::build_before_setup(
        repo,
        state,
        &second_allocation,
        Some(project),
    ) {
        Ok(environment) => environment,
        Err(error) => {
            findings.push(ComposeFinding {
                code: ComposeFindingCode::UnsafeSyntheticPort,
                service: None,
                field: "environment".to_string(),
                observed: vec![error.to_string()],
            });
            return ComposeReport::new(findings);
        }
    };
    let root = Path::new(&allocation.path);
    let first = render_and_parse(
        root,
        spec,
        first_environment,
        "first",
        &mut render,
        &mut findings,
    );
    let second = render_and_parse(
        root,
        spec,
        &second_environment,
        "second",
        &mut render,
        &mut findings,
    );
    if let (Some(first), Some(second)) = (first, second) {
        compare(&first, &second, &mut findings);
    }
    ComposeReport::new(findings)
}

fn synthetic_allocation(
    state: &State,
    allocation: &Allocation,
    project: &ProjectConfig,
) -> Result<Allocation> {
    let request = crate::state::ReservationRequest::compose_probe(project, allocation)?;
    let reservation = crate::state::reserve_ports(state, &request)?;
    let mut synthetic = allocation.clone();
    synthetic.name = format!("{}-compose-probe", allocation.name);
    synthetic.block = reservation.block;
    synthetic.offset = reservation.offset;
    synthetic.ports = reservation.ports;
    Ok(synthetic)
}

fn render_compose(
    worktree: &Path,
    spec: &ComposeSpec,
    environment: &ResolvedEnvironment,
) -> Result<Vec<u8>> {
    let mut command = Command::new("docker");
    command.arg("compose");
    for file in spec.files() {
        command.arg("-f").arg(file);
    }
    command.args(["config", "--format", "json"]);
    command.current_dir(worktree);
    environment.apply_to(&mut command);
    run_command_with_timeout(&mut command, Duration::from_secs(30))
}

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut stdout = tempfile::tempfile().context("create Compose output file")?;
    let mut child = command
        .stdout(Stdio::from(
            stdout.try_clone().context("clone Compose output file")?,
        ))
        .stderr(Stdio::null())
        .spawn()
        .context("run docker compose config")?;
    let status = match child
        .wait_timeout(timeout)
        .context("wait for docker compose config")?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            child
                .wait()
                .context("reap timed out docker compose config")?;
            return Err(anyhow!(
                "docker compose config timed out after {} seconds",
                timeout.as_secs_f64()
            ));
        }
    };
    if !status.success() {
        return Err(anyhow!("docker compose config exited with {status}"));
    }
    stdout
        .seek(SeekFrom::Start(0))
        .context("rewind Docker Compose output")?;
    let mut bytes = Vec::new();
    stdout
        .read_to_end(&mut bytes)
        .context("read Docker Compose output")?;
    Ok(bytes)
}

fn render_and_parse<F>(
    root: &Path,
    spec: &ComposeSpec,
    environment: &ResolvedEnvironment,
    label: &str,
    render: &mut F,
    findings: &mut Vec<ComposeFinding>,
) -> Option<RenderedCompose>
where
    F: FnMut(&Path, &ComposeSpec, &ResolvedEnvironment) -> Result<Vec<u8>>,
{
    let bytes = match render(root, spec, environment) {
        Ok(bytes) => bytes,
        Err(error) => {
            findings.push(ComposeFinding {
                code: ComposeFindingCode::RenderFailed,
                service: None,
                field: label.to_string(),
                observed: vec![error.to_string()],
            });
            return None;
        }
    };
    match parse_rendered(&bytes) {
        Ok(rendered) => {
            find_duplicate_ports(&rendered, label, findings);
            Some(rendered)
        }
        Err(error) => {
            findings.push(ComposeFinding {
                code: ComposeFindingCode::MalformedOutput,
                service: None,
                field: label.to_string(),
                observed: vec![error.to_string()],
            });
            None
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RenderedCompose {
    services: BTreeMap<String, RenderedService>,
}

#[derive(Debug, Eq, PartialEq)]
struct RenderedService {
    container_name: Option<String>,
    port_groups: BTreeMap<PortGroup, Vec<String>>,
    bindings: Vec<PublishedBinding>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PortGroup {
    target: String,
    protocol: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublishedBinding {
    host_ip: String,
    published: String,
    protocol: String,
    target: String,
}

fn parse_rendered(bytes: &[u8]) -> Result<RenderedCompose> {
    let value: Value = serde_json::from_slice(bytes).context("parse Compose JSON")?;
    let services = value
        .get("services")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Compose JSON has no services object"))?;
    if services.is_empty() {
        return Err(anyhow!("Compose JSON services object is empty"));
    }
    let services = services
        .iter()
        .map(|(name, value)| parse_service(name, value).map(|service| (name.clone(), service)))
        .collect::<Result<_>>()?;
    Ok(RenderedCompose { services })
}

fn parse_service(name: &str, value: &Value) -> Result<RenderedService> {
    let service = value
        .as_object()
        .ok_or_else(|| anyhow!("service {name:?} is not an object"))?;
    let container_name = match service.get("container_name") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.trim().is_empty() => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(anyhow!("service {name:?} container_name is not a string")),
    };
    let mut bindings = Vec::new();
    if let Some(ports) = service.get("ports") {
        let ports = ports
            .as_array()
            .ok_or_else(|| anyhow!("service {name:?} ports is not an array"))?;
        for port in ports {
            let port = port
                .as_object()
                .ok_or_else(|| anyhow!("service {name:?} has a malformed port entry"))?;
            let Some(published) = port.get("published") else {
                continue;
            };
            let published = scalar(published)
                .with_context(|| format!("service {name:?} has invalid published port"))?;
            let target = scalar(
                port.get("target")
                    .ok_or_else(|| anyhow!("service {name:?} published port has no target"))?,
            )
            .with_context(|| format!("service {name:?} has invalid target port"))?;
            let protocol = port
                .get("protocol")
                .map(scalar)
                .transpose()
                .with_context(|| format!("service {name:?} has invalid port protocol"))?
                .unwrap_or_else(|| "tcp".to_string());
            let host_ip = port
                .get("host_ip")
                .map(scalar)
                .transpose()
                .with_context(|| format!("service {name:?} has invalid host_ip"))?
                .unwrap_or_default();
            bindings.push(PublishedBinding {
                host_ip,
                published,
                protocol,
                target,
            });
        }
    }
    bindings.sort_by(|left, right| {
        (&left.target, &left.protocol, &left.published, &left.host_ip).cmp(&(
            &right.target,
            &right.protocol,
            &right.published,
            &right.host_ip,
        ))
    });
    let mut port_groups = BTreeMap::<PortGroup, Vec<String>>::new();
    for binding in &bindings {
        port_groups
            .entry(PortGroup {
                target: binding.target.clone(),
                protocol: binding.protocol.clone(),
            })
            .or_default()
            .push(binding.published.clone());
    }
    Ok(RenderedService {
        container_name,
        port_groups,
        bindings,
    })
}

fn scalar(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(anyhow!("expected a string or number")),
    }
}

fn find_duplicate_ports(
    rendered: &RenderedCompose,
    label: &str,
    findings: &mut Vec<ComposeFinding>,
) {
    let mut claimed = BTreeMap::<(String, String), Vec<(String, String, String)>>::new();
    let mut reported = BTreeSet::new();
    for (service, config) in &rendered.services {
        for binding in &config.bindings {
            if binding.published == "0" {
                continue;
            }
            let key = (binding.published.clone(), binding.protocol.clone());
            let overlap = claimed.get(&key).and_then(|claims| {
                claims
                    .iter()
                    .find(|(_, _, host_ip)| host_ips_overlap(host_ip, &binding.host_ip))
            });
            if let Some((other_service, other_target, _)) = overlap
                && reported.insert((
                    binding.published.clone(),
                    binding.protocol.clone(),
                    service.clone(),
                ))
            {
                findings.push(ComposeFinding {
                    code: ComposeFindingCode::DuplicateHostPort,
                    service: Some(service.clone()),
                    field: format!("ports.{}", binding.target),
                    observed: vec![
                        label.to_string(),
                        binding.published.clone(),
                        format!("{other_service}:{other_target}"),
                    ],
                });
            }
            claimed.entry(key).or_default().push((
                service.clone(),
                binding.target.clone(),
                binding.host_ip.clone(),
            ));
        }
    }
}

fn host_ips_overlap(first: &str, second: &str) -> bool {
    first == second || is_wildcard_host(first) || is_wildcard_host(second)
}

fn is_wildcard_host(host: &str) -> bool {
    if host.is_empty() {
        return true;
    }
    host.trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_unspecified())
}

fn compare(first: &RenderedCompose, second: &RenderedCompose, findings: &mut Vec<ComposeFinding>) {
    compare_container_names(first, second, findings);
    compare_host_ports(first, second, findings);
    let services = first
        .services
        .keys()
        .chain(second.services.keys())
        .collect::<BTreeSet<_>>();
    for service in services {
        let Some(first_service) = first.services.get(service) else {
            findings.push(shape_finding(service, "service", "missing", "present"));
            continue;
        };
        let Some(second_service) = second.services.get(service) else {
            findings.push(shape_finding(service, "service", "present", "missing"));
            continue;
        };
        let groups = first_service
            .port_groups
            .keys()
            .chain(second_service.port_groups.keys())
            .collect::<BTreeSet<_>>();
        for group in groups {
            let first_ports = first_service.port_groups.get(group);
            let second_ports = second_service.port_groups.get(group);
            if first_ports.map(Vec::len) == second_ports.map(Vec::len) {
                continue;
            }
            let count = |ports: Option<&Vec<String>>| {
                ports.map_or_else(|| "missing".to_string(), |ports| ports.len().to_string())
            };
            findings.push(shape_finding(
                service,
                &format!("ports.{}.{}", group.target, group.protocol),
                &count(first_ports),
                &count(second_ports),
            ));
        }
    }
}

fn compare_container_names(
    first: &RenderedCompose,
    second: &RenderedCompose,
    findings: &mut Vec<ComposeFinding>,
) {
    let first_names = first
        .services
        .iter()
        .filter_map(|(service, config)| {
            config
                .container_name
                .as_deref()
                .map(|name| (name, service.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    for (service, config) in &second.services {
        let Some(name) = config.container_name.as_deref() else {
            continue;
        };
        let Some(first_service) = first_names.get(name) else {
            continue;
        };
        findings.push(ComposeFinding {
            code: ComposeFindingCode::FixedContainerName,
            service: Some(service.clone()),
            field: "container_name".to_string(),
            observed: vec![
                name.to_string(),
                format!("first:{first_service}"),
                format!("second:{service}"),
            ],
        });
    }
}

fn compare_host_ports(
    first: &RenderedCompose,
    second: &RenderedCompose,
    findings: &mut Vec<ComposeFinding>,
) {
    for (second_service, second_config) in &second.services {
        for second_binding in &second_config.bindings {
            if second_binding.published == "0" {
                continue;
            }
            let overlap = first
                .services
                .iter()
                .find_map(|(first_service, first_config)| {
                    first_config
                        .bindings
                        .iter()
                        .find(|first_binding| {
                            first_binding.published == second_binding.published
                                && first_binding.protocol == second_binding.protocol
                                && host_ips_overlap(&first_binding.host_ip, &second_binding.host_ip)
                        })
                        .map(|binding| (first_service, binding))
                });
            let Some((first_service, first_binding)) = overlap else {
                continue;
            };
            findings.push(ComposeFinding {
                code: ComposeFindingCode::FixedHostPort,
                service: Some(second_service.clone()),
                field: format!(
                    "ports.{}.{}",
                    second_binding.target, second_binding.protocol
                ),
                observed: vec![
                    second_binding.published.clone(),
                    format!("first:{first_service}:{}", first_binding.target),
                    format!("second:{second_service}:{}", second_binding.target),
                ],
            });
        }
    }
}

fn shape_finding(service: &str, field: &str, first: &str, second: &str) -> ComposeFinding {
    ComposeFinding {
        code: ComposeFindingCode::ChangedOutputShape,
        service: Some(service.to_string()),
        field: field.to_string(),
        observed: vec![first.to_string(), second.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectConfig;
    use crate::state::{AllocationGeneration, AllocationSetup, SupabaseAllocation};

    fn project() -> ProjectConfig {
        ProjectConfig::from_slice(
            br#"{
              "version":2,
              "port_stride":100,
              "ports":[
                {"key":"postgres","base_port":5432,"outputs":[{"env":"POSTGRES_PORT","template":"{port}"}]},
                {"key":"redis","base_port":6379,"outputs":[{"env":"REDIS_PORT","template":"{port}"}]}
              ],
              "commands":{"setup":null,"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},
              "compose":{"files":["compose.yaml"]},
              "supabase":null
            }"#,
        )
        .unwrap()
    }

    fn allocation(root: &Path, project: &ProjectConfig, offset: i32) -> Allocation {
        Allocation {
            generation_id: AllocationGeneration::new(),
            name: "feature".to_string(),
            branch: "feature".to_string(),
            path: root.to_string_lossy().to_string(),
            block: offset / 100,
            offset,
            ports: crate::state::project_port_assignments(project, offset).unwrap(),
            status: "creating".to_string(),
            created_at: "now".to_string(),
            supabase: SupabaseAllocation::None,
            setup: AllocationSetup::default(),
        }
    }

    fn repo(root: &Path) -> Repo {
        Repo::new(
            root.to_path_buf(),
            root.join(".git"),
            root.to_path_buf(),
            root.to_path_buf(),
            Some(root.to_path_buf()),
        )
    }

    fn json(postgres: u16, redis: u16, container: Option<&str>, reverse: bool) -> Vec<u8> {
        let mut ports = vec![
            serde_json::json!({"target":5432,"published":postgres.to_string(),"protocol":"tcp"}),
            serde_json::json!({"target":5432,"published":(postgres + 1).to_string(),"protocol":"tcp"}),
        ];
        if reverse {
            ports.reverse();
        }
        serde_json::to_vec(&serde_json::json!({
            "services": {
                "postgres": {"container_name":container,"ports":ports},
                "redis": {"ports":[{"target":6379,"published":redis,"protocol":"tcp"}]}
            }
        }))
        .unwrap()
    }

    #[test]
    fn safe_render_passes_when_json_order_changes() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = project();
        let allocation = allocation(temp.path(), &project, 100);
        let repo = repo(temp.path());
        let state = State::empty();
        let environment =
            ResolvedEnvironment::build_before_setup(&repo, &state, &allocation, Some(&project))
                .unwrap();
        let mut outputs = [json(5532, 6479, None, false), json(5632, 6579, None, true)].into_iter();
        let report = inspect_with_renderer(
            &repo,
            &state,
            &allocation,
            &project,
            project.compose().unwrap(),
            &environment,
            |_, _, _| Ok(outputs.next().unwrap()),
        );
        assert_eq!(report, ComposeReport::new(Vec::new()));
    }

    #[test]
    fn synthetic_probe_uses_a_lower_free_block_when_current_is_at_the_limit() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = project();
        let current = allocation(temp.path(), &project, 59_000);
        let mut occupied = allocation(temp.path(), &project, 100);
        occupied.name = "occupied".to_string();
        occupied.branch = "occupied".to_string();
        let mut state = State::empty();
        state
            .allocations
            .insert(current.name.clone(), current.clone());
        state.allocations.insert(occupied.name.clone(), occupied);

        let probe = synthetic_allocation(&state, &current, &project).unwrap();

        assert_eq!(probe.block, 2);
        assert_eq!(probe.offset, 200);
    }

    #[test]
    fn fixed_ports_container_names_and_duplicates_are_reported() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = project();
        let allocation = allocation(temp.path(), &project, 100);
        let repo = repo(temp.path());
        let state = State::empty();
        let environment =
            ResolvedEnvironment::build_before_setup(&repo, &state, &allocation, Some(&project))
                .unwrap();
        let duplicate = serde_json::to_vec(&serde_json::json!({"services":{
            "postgres":{"container_name":"eln-postgres","ports":[{"target":5432,"published":"5432","protocol":"tcp"}]},
            "redis":{"ports":[{"target":6379,"published":"5432","protocol":"tcp"}]}
        }}))
        .unwrap();
        let mut outputs = [duplicate.clone(), duplicate].into_iter();
        let report = inspect_with_renderer(
            &repo,
            &state,
            &allocation,
            &project,
            project.compose().unwrap(),
            &environment,
            |_, _, _| Ok(outputs.next().unwrap()),
        );
        let codes = report
            .findings()
            .iter()
            .map(|finding| finding.code)
            .collect::<Vec<_>>();
        assert!(codes.contains(&ComposeFindingCode::DuplicateHostPort));
        assert!(codes.contains(&ComposeFindingCode::FixedHostPort));
        assert!(codes.contains(&ComposeFindingCode::FixedContainerName));
    }

    #[test]
    fn render_failure_and_malformed_output_block() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = project();
        let allocation = allocation(temp.path(), &project, 100);
        let repo = repo(temp.path());
        let state = State::empty();
        let environment =
            ResolvedEnvironment::build_before_setup(&repo, &state, &allocation, Some(&project))
                .unwrap();
        let mut call = 0;
        let report = inspect_with_renderer(
            &repo,
            &state,
            &allocation,
            &project,
            project.compose().unwrap(),
            &environment,
            |_, _, _| {
                call += 1;
                if call == 1 {
                    Err(anyhow!("mock Docker failure"))
                } else {
                    Ok(b"not json".to_vec())
                }
            },
        );
        assert_eq!(report.findings().len(), 2);
        assert_eq!(report.findings()[0].code, ComposeFindingCode::RenderFailed);
        assert_eq!(
            report.findings()[1].code,
            ComposeFindingCode::MalformedOutput
        );
    }

    #[test]
    fn repeated_binding_groups_detect_common_nonzero_ports_but_ignore_zero() {
        let first = parse_rendered(
            &serde_json::to_vec(&serde_json::json!({"services":{"api":{"ports":[
                {"target":8080,"published":"0","protocol":"tcp"},
                {"target":8080,"published":"1000","protocol":"tcp"},
                {"target":8080,"published":"2000","protocol":"tcp"}
            ]}}}))
            .unwrap(),
        )
        .unwrap();
        let second = parse_rendered(
            &serde_json::to_vec(&serde_json::json!({"services":{"api":{"ports":[
                {"target":8080,"published":"3000","protocol":"tcp"},
                {"target":8080,"published":"2000","protocol":"tcp"},
                {"target":8080,"published":"0","protocol":"tcp"}
            ]}}}))
            .unwrap(),
        )
        .unwrap();
        let mut findings = Vec::new();
        compare(&first, &second, &mut findings);
        let fixed = findings
            .iter()
            .filter(|finding| finding.code == ComposeFindingCode::FixedHostPort)
            .collect::<Vec<_>>();
        assert_eq!(fixed.len(), 1, "{findings:?}");
        assert_eq!(fixed[0].observed[0], "2000");
    }

    #[test]
    fn cross_service_resource_intersections_are_reported() {
        let first = parse_rendered(
            &serde_json::to_vec(&serde_json::json!({"services":{
                "api":{"container_name":"shared-name","ports":[{"target":8080,"published":"9000","protocol":"tcp"}]}
            }}))
            .unwrap(),
        )
        .unwrap();
        let second = parse_rendered(
            &serde_json::to_vec(&serde_json::json!({"services":{
                "web":{"container_name":"shared-name","ports":[{"target":80,"published":"9000","protocol":"tcp"}]}
            }}))
            .unwrap(),
        )
        .unwrap();
        let mut findings = Vec::new();
        compare(&first, &second, &mut findings);
        assert!(findings.iter().any(|finding| {
            finding.code == ComposeFindingCode::FixedHostPort
                && finding.service.as_deref() == Some("web")
                && finding.observed[0] == "9000"
        }));
        assert!(findings.iter().any(|finding| {
            finding.code == ComposeFindingCode::FixedContainerName
                && finding.service.as_deref() == Some("web")
                && finding.observed[0] == "shared-name"
        }));
    }

    #[test]
    fn duplicate_ports_apply_wildcard_host_overlap_rules() {
        let rendered = parse_rendered(
            &serde_json::to_vec(&serde_json::json!({"services":{
                "wildcard":{"ports":[{"target":8080,"published":"9000","protocol":"tcp"}]},
                "loopback":{"ports":[{"target":8081,"published":"9000","protocol":"tcp","host_ip":"127.0.0.1"}]},
                "ipv6-wildcard":{"ports":[{"target":8086,"published":"9002","protocol":"tcp","host_ip":"0:0:0:0:0:0:0:0"}]},
                "ipv6-loopback":{"ports":[{"target":8087,"published":"9002","protocol":"tcp","host_ip":"::1"}]},
                "private-a":{"ports":[{"target":8082,"published":"9001","protocol":"tcp","host_ip":"127.0.0.1"}]},
                "private-b":{"ports":[{"target":8083,"published":"9001","protocol":"tcp","host_ip":"192.0.2.10"}]},
                "random-a":{"ports":[{"target":8084,"published":"0","protocol":"tcp"}]},
                "random-b":{"ports":[{"target":8085,"published":"0","protocol":"tcp"}]}
            }}))
            .unwrap(),
        )
        .unwrap();
        let mut findings = Vec::new();
        find_duplicate_ports(&rendered, "first", &mut findings);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(
            findings
                .iter()
                .all(|finding| finding.code == ComposeFindingCode::DuplicateHostPort)
        );
        let ports = findings
            .iter()
            .map(|finding| finding.observed[1].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(ports, BTreeSet::from(["9000", "9002"]));
    }

    #[test]
    fn unsafe_synthetic_port_blocks_before_rendering() {
        let project = ProjectConfig::from_slice(
            br#"{
              "version":2,"port_stride":100,
              "ports":[{"key":"api","base_port":65530,"outputs":[]}],
              "commands":{"setup":null,"start":null,"stop":null,"status":null,"db_migrate":null,"db_seed":null,"db_reset":null},
              "compose":{"files":["compose.yaml"]},"supabase":null
            }"#,
        )
        .unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        let allocation = allocation(temp.path(), &project, 0);
        let repo = repo(temp.path());
        let state = State::empty();
        let environment =
            ResolvedEnvironment::build_before_setup(&repo, &state, &allocation, Some(&project))
                .unwrap();
        let mut rendered = false;
        let report = inspect_with_renderer(
            &repo,
            &state,
            &allocation,
            &project,
            project.compose().unwrap(),
            &environment,
            |_, _, _| {
                rendered = true;
                Ok(Vec::new())
            },
        );
        assert!(!rendered);
        assert_eq!(
            report.findings()[0].code,
            ComposeFindingCode::UnsafeSyntheticPort
        );
    }

    #[cfg(unix)]
    #[test]
    fn compose_command_timeout_kills_and_reaps_the_child() {
        let mut command = Command::new("sleep");
        command.arg("5");
        let error = run_command_with_timeout(&mut command, Duration::from_millis(20)).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
    }
}
