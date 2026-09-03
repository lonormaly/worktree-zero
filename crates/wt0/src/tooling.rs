//! Read-only detection of the frameworks and dev-loop tools a repository
//! uses, shared by `wt0 doctor`'s before/after report and `wt0 init tilt`'s
//! proposal. Every check is a file/content probe — nothing here runs a
//! project command or writes anything.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Frameworks and build tools detected from tracked files, independent of
/// any package-manager classification `runtime::dependency_facts` already
/// covers.
pub(crate) struct ToolingReport {
    pub(crate) next: bool,
    pub(crate) nx: bool,
    pub(crate) turbo: bool,
    pub(crate) cargo: bool,
    pub(crate) tilt: bool,
    pub(crate) portless: bool,
    pub(crate) compose: bool,
    pub(crate) k8s: bool,
}

impl ToolingReport {
    /// Display/JSON names for every tool detected, in a fixed, readable order.
    pub(crate) fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.next {
            names.push("Next.js");
        }
        if self.nx {
            names.push("Nx");
        }
        if self.turbo {
            names.push("Turbo");
        }
        if self.cargo {
            names.push("Cargo");
        }
        if self.tilt {
            names.push("Tilt");
        }
        if self.portless {
            names.push("Portless");
        }
        if self.compose {
            names.push("docker-compose");
        }
        if self.k8s {
            names.push("Kubernetes manifests");
        }
        names
    }
}

pub(crate) fn detect(root: &Path) -> ToolingReport {
    let tiltfiles = tiltfile_paths(root);
    let tilt = !tiltfiles.is_empty() || root.join("tilt_up.sh").is_file();
    let combined = tilt_related_text(root);
    ToolingReport {
        next: package_json_has_dependency(root, "next"),
        nx: root.join("nx.json").is_file(),
        turbo: root.join("turbo.json").is_file(),
        cargo: root.join("Cargo.toml").is_file(),
        tilt,
        portless: package_json_scripts_mention(root, "portless") || combined.contains(".localhost"),
        compose: [
            "compose.yaml",
            "compose.yml",
            "docker-compose.yml",
            "docker-compose.yaml",
        ]
        .iter()
        .any(|name| root.join(name).is_file()),
        k8s: root.join("k8s").is_dir() || tracked_file_matches(root, is_k8s_manifest_name),
    }
}

/// What `wt0 doctor` and `wt0 init tilt` both need about an existing Tilt
/// setup: whether one exists, any hard-coded ports/hostnames found in it, and
/// whether it already derives them from wt0's own per-runtime identity
/// (`WT0_PORT_BASE`, `WT0_SLUG`) — the pattern FLAM's `.wt0/hooks/post-create`
/// and Builders Stack's `tilt_up.sh`/`.devops/Tiltfile` both use.
pub(crate) struct TiltReport {
    pub(crate) detected: bool,
    pub(crate) literal_ports: Vec<String>,
    pub(crate) literal_hosts: Vec<String>,
    pub(crate) derives_from_wt0: bool,
}

pub(crate) fn detect_tilt(root: &Path) -> TiltReport {
    let detected = !tiltfile_paths(root).is_empty() || root.join("tilt_up.sh").is_file();
    if !detected {
        return TiltReport {
            detected: false,
            literal_ports: Vec::new(),
            literal_hosts: Vec::new(),
            derives_from_wt0: false,
        };
    }
    let combined = tilt_related_text(root);
    TiltReport {
        detected: true,
        literal_ports: extract_literal_ports(&combined),
        literal_hosts: extract_tokens_containing(&combined, ".localhost"),
        derives_from_wt0: combined.contains("WT0_PORT_BASE") || combined.contains("WT0_SLUG"),
    }
}

const TILTFILE_CANDIDATES: &[&str] = &[
    "Tiltfile",
    ".devops/Tiltfile",
    "tilt/Tiltfile",
    "deploy/Tiltfile",
];

pub(crate) fn tiltfile_paths(root: &Path) -> Vec<PathBuf> {
    TILTFILE_CANDIDATES
        .iter()
        .map(|relative| root.join(relative))
        .filter(|path| path.is_file())
        .collect()
}

/// Best-effort concatenation of every Tiltfile and boot/stop script this
/// repository ships, for a single text scan. Unreadable files are skipped,
/// never an error — detection degrades, it never blocks `doctor`.
fn tilt_related_text(root: &Path) -> String {
    let mut combined = String::new();
    for path in tiltfile_paths(root) {
        if let Ok(text) = fs::read_to_string(&path) {
            combined.push('\n');
            combined.push_str(&text);
        }
    }
    for name in ["tilt_up.sh", "tilt_down.sh"] {
        if let Ok(text) = fs::read_to_string(root.join(name)) {
            combined.push('\n');
            combined.push_str(&text);
        }
    }
    combined
}

/// Distinct 4–5 digit numbers on any line that mentions "port"
/// (case-insensitive) — a cheap, deliberately approximate scan for hard-coded
/// ports; false positives (a port number in a comment) are harmless here,
/// since this only ever feeds an informational report, never a refusal.
fn extract_literal_ports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        if !line.to_ascii_lowercase().contains("port") {
            continue;
        }
        let bytes = line.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index].is_ascii_digit() {
                let start = index;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                let token = &line[start..index];
                if (4..=5).contains(&token.len()) && !found.iter().any(|existing| existing == token)
                {
                    found.push(token.to_owned());
                }
            } else {
                index += 1;
            }
        }
    }
    found
}

/// Distinct whitespace/quote/paren-delimited tokens containing `needle`.
fn extract_tokens_containing(text: &str, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for raw in text.split(|character: char| {
        character.is_whitespace() || matches!(character, '\'' | '"' | '(' | ')' | ',')
    }) {
        let token = raw.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '.' | '_' | '-' | ':')
        });
        if token.contains(needle) && !found.iter().any(|existing| existing == token) {
            found.push(token.to_owned());
        }
    }
    found
}

fn package_json(root: &Path) -> Option<Value> {
    let text = fs::read_to_string(root.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn package_json_has_dependency(root: &Path, name: &str) -> bool {
    let Some(value) = package_json(root) else {
        return false;
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .any(|key| value.get(key).and_then(|deps| deps.get(name)).is_some())
}

fn package_json_scripts_mention(root: &Path, needle: &str) -> bool {
    let Some(value) = package_json(root) else {
        return false;
    };
    value
        .get("scripts")
        .and_then(Value::as_object)
        .is_some_and(|scripts| {
            scripts
                .values()
                .any(|script| script.as_str().is_some_and(|text| text.contains(needle)))
        })
}

fn is_k8s_manifest_name(name: &str) -> bool {
    name.ends_with(".k8s.yaml") || name.ends_with(".k8s.yml")
}

/// Whether any Git-tracked path's file name satisfies `predicate` — the same
/// `git ls-files` probe `capabilities::has_named_file` uses for extension
/// matching instead of exact names.
fn tracked_file_matches(root: &Path, predicate: impl Fn(&str) -> bool) -> bool {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output();
    output.ok().is_some_and(|output| {
        output.status.success()
            && output.stdout.split(|byte| *byte == 0).any(|raw| {
                std::str::from_utf8(raw)
                    .ok()
                    .and_then(|path| Path::new(path).file_name())
                    .and_then(|name| name.to_str())
                    .is_some_and(&predicate)
            })
    })
}

// ---------------------------------------------------------------------------
// Dev environments: not everyone uses Tilt
// ---------------------------------------------------------------------------

/// One dev-environment tool detected in a repository, generalizing
/// `TiltReport` so `wt0 doctor` gives the same "two agents will collide,
/// here's the fix" advice whichever tool a project actually boots its dev
/// stack with — Tilt is one option among several. One entry per detected
/// tool, in a fixed, readable order (`detect_dev_environment`).
pub(crate) struct DevTool {
    pub(crate) tool: String,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) literal_ports: Vec<String>,
    pub(crate) literal_hosts: Vec<String>,
    pub(crate) derives_from_wt0: bool,
    /// One or two lines: the concrete fix for *this* tool, worded for the
    /// tool detected — shared verbatim by `wt0 doctor`'s text report and its
    /// `--json` `dev_environment` field so the two can never drift apart.
    pub(crate) fix: Vec<String>,
}

/// Detects every dev-environment tool this repository boots a stack with —
/// Tilt (via `detect_tilt`, reused so both stay in lockstep), docker-compose,
/// a devcontainer, a Procfile-style process manager, a cloud-native dev tool
/// (Skaffold/Garden/DevSpace), or a plain `package.json` dev script — and
/// reports each one's hard-coded ports/hostnames plus the fix for that
/// specific tool. Every check is a file/content probe, same as `detect`
/// above: nothing here runs a project command.
pub(crate) fn detect_dev_environment(root: &Path) -> Vec<DevTool> {
    [
        dev_tool_tilt(root),
        dev_tool_compose(root),
        dev_tool_devcontainer(root),
        dev_tool_process_manager(root),
        dev_tool_cloud_native(root),
        dev_tool_plain_scripts(root),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn dev_tool_tilt(root: &Path) -> Option<DevTool> {
    let tilt = detect_tilt(root);
    if !tilt.detected {
        return None;
    }
    Some(DevTool {
        tool: "Tilt".to_owned(),
        files: tiltfile_paths(root),
        literal_ports: tilt.literal_ports,
        literal_hosts: tilt.literal_hosts,
        derives_from_wt0: tilt.derives_from_wt0,
        fix: vec![
            "wt0 init tilt — Tiltfile snippet deriving TILT_PORT from WT0_PORT_BASE".to_owned(),
        ],
    })
}

const COMPOSE_CANDIDATES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yml",
    "docker-compose.yaml",
];

pub(crate) fn compose_paths(root: &Path) -> Vec<PathBuf> {
    COMPOSE_CANDIDATES
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect()
}

fn dev_tool_compose(root: &Path) -> Option<DevTool> {
    let files = compose_paths(root);
    if files.is_empty() {
        return None;
    }
    let combined = concat_files(&files);
    let literal_ports = compose_host_ports(&combined);
    let literal_hosts = compose_container_names(&combined);
    let derives_from_wt0 = combined.contains("WT0_PORT_BASE") || combined.contains("WT0_SLUG");
    Some(DevTool {
        tool: "docker-compose".to_owned(),
        files,
        literal_ports,
        literal_hosts,
        derives_from_wt0,
        fix: vec![
            "wt0 init compose — compose.wt0.yaml sets COMPOSE_PROJECT_NAME=${WT0_SLUG:-local}"
                .to_owned(),
            "and derives host ports from WT0_PORT_BASE".to_owned(),
        ],
    })
}

/// Distinct 2–5 digit host-port tokens found on lines inside a `ports:`
/// block — deliberately approximate line/indent scanning rather than a full
/// YAML parse (same trade-off as `extract_literal_ports` above): this only
/// ever feeds an informational report, never a refusal.
fn compose_host_ports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_ports = false;
    let mut ports_indent = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.starts_with("ports:") {
            in_ports = true;
            ports_indent = indent;
            continue;
        }
        if !in_ports || trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('-') {
            in_ports = indent > ports_indent;
            continue;
        }
        for token in trimmed.split(|character: char| !character.is_ascii_digit()) {
            if (2..=5).contains(&token.len()) && !found.iter().any(|existing| existing == token) {
                found.push(token.to_owned());
            }
        }
    }
    found
}

fn compose_container_names(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("container_name:") else {
            continue;
        };
        let name = rest
            .trim()
            .trim_matches(|character| matches!(character, '"' | '\''));
        if !name.is_empty() && !found.iter().any(|existing| existing == name) {
            found.push(name.to_owned());
        }
    }
    found
}

/// `(service name, [(host port, container port)])` for every service under
/// `services:` that has at least one literal `ports:` entry, in file order —
/// what `wt0 init compose` proposes one override variable per port for.
/// Same line/indent scanning trade-off as `compose_host_ports`.
pub(crate) fn compose_service_ports(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut services: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut in_services = false;
    let mut services_indent = 0usize;
    let mut service_level: Option<usize> = None;
    let mut current: Option<usize> = None;
    let mut in_ports = false;
    let mut ports_indent = 0usize;

    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !in_services {
            if trimmed.starts_with("services:") {
                in_services = true;
                services_indent = indent;
            }
            continue;
        }
        if indent <= services_indent {
            in_services = false;
            current = None;
            in_ports = false;
            continue;
        }
        let level = *service_level.get_or_insert(indent);
        if indent == level && !trimmed.starts_with('-') && trimmed.ends_with(':') {
            services.push((trimmed.trim_end_matches(':').to_owned(), Vec::new()));
            current = Some(services.len() - 1);
            in_ports = false;
            continue;
        }
        let Some(index) = current else { continue };
        if trimmed.starts_with("ports:") {
            in_ports = true;
            ports_indent = indent;
            continue;
        }
        if !in_ports {
            continue;
        }
        if !trimmed.starts_with('-') {
            in_ports = indent > ports_indent;
            continue;
        }
        let digits: Vec<&str> = trimmed
            .split(|character: char| !character.is_ascii_digit())
            .filter(|token| (2..=5).contains(&token.len()))
            .collect();
        if let Some(&host) = digits.first() {
            let container = digits.last().copied().unwrap_or(host);
            let pair = (host.to_owned(), container.to_owned());
            if !services[index].1.contains(&pair) {
                services[index].1.push(pair);
            }
        }
    }
    services
        .into_iter()
        .filter(|(_, ports)| !ports.is_empty())
        .collect()
}

fn dev_tool_devcontainer(root: &Path) -> Option<DevTool> {
    let path = root.join(".devcontainer/devcontainer.json");
    if !path.is_file() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    let parsed: Option<Value> = serde_json::from_str(&strip_json_comments(&raw)).ok();
    let literal_ports = parsed
        .as_ref()
        .and_then(|value| value.get("forwardPorts"))
        .and_then(Value::as_array)
        .map(|ports| {
            ports
                .iter()
                .filter_map(|port| {
                    port.as_u64()
                        .map(|number| number.to_string())
                        .or_else(|| port.as_str().map(str::to_owned))
                })
                .collect()
        })
        .unwrap_or_default();
    let derives_from_wt0 = raw.contains("WT0_PORT_BASE") || raw.contains("WT0_SLUG");
    Some(DevTool {
        tool: "devcontainer".to_owned(),
        files: vec![path],
        literal_ports,
        literal_hosts: Vec::new(),
        derives_from_wt0,
        fix: vec![
            "wt0 init dev — post-create hook exports PORT=$WT0_PORT_BASE".to_owned(),
            "reference it from postCreateCommand instead of a literal forwardPorts entry"
                .to_owned(),
        ],
    })
}

/// Strips `//` line comments from JSONC (devcontainer.json's format),
/// tracking quote state so a `//` inside a string value (a URL, say) is left
/// alone. Best-effort: does not handle `/* */` block comments or trailing
/// commas, since detection here only ever feeds an informational report —
/// a parse failure just means an empty `literal_ports` list, not an error.
fn strip_json_comments(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut in_string = false;
        let mut escaped = false;
        let mut cut = None;
        for (index, &character) in chars.iter().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
            } else if character == '"' {
                in_string = true;
            } else if character == '/' && chars.get(index + 1) == Some(&'/') {
                cut = Some(index);
                break;
            }
        }
        match cut {
            Some(index) => result.extend(&chars[..index]),
            None => result.extend(&chars),
        }
        result.push('\n');
    }
    result
}

fn dev_tool_process_manager(root: &Path) -> Option<DevTool> {
    let mut files: Vec<PathBuf> = ["Procfile", "mprocs.yaml", "mprocs.yml"]
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect();
    if package_json_scripts_mention(root, "concurrently") {
        let package_json_path = root.join("package.json");
        if package_json_path.is_file() {
            files.push(package_json_path);
        }
    }
    if files.is_empty() {
        return None;
    }
    let combined = concat_files(&files);
    let literal_ports = extract_flag_ports(&combined);
    let derives_from_wt0 = combined.contains("WT0_PORT_BASE") || combined.contains("WT0_SLUG");
    Some(DevTool {
        tool: "Procfile / process manager".to_owned(),
        files,
        literal_ports,
        literal_hosts: Vec::new(),
        derives_from_wt0,
        fix: vec![
            "wt0 init dev — post-create hook exports PORT=$WT0_PORT_BASE".to_owned(),
            "and writes .env.wt0 for the Procfile/mprocs command to source".to_owned(),
        ],
    })
}

const CLOUD_NATIVE_CANDIDATES: &[&str] = &[
    "skaffold.yaml",
    "skaffold.yml",
    "garden.yml",
    "garden.yaml",
    "devspace.yaml",
    "devspace.yml",
];

fn dev_tool_cloud_native(root: &Path) -> Option<DevTool> {
    let files: Vec<PathBuf> = CLOUD_NATIVE_CANDIDATES
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect();
    if files.is_empty() {
        return None;
    }
    let combined = concat_files(&files);
    let literal_ports = extract_literal_ports(&combined);
    let derives_from_wt0 = combined.contains("WT0_PORT_BASE") || combined.contains("WT0_SLUG");
    Some(DevTool {
        tool: "Skaffold / Garden / DevSpace".to_owned(),
        files,
        literal_ports,
        literal_hosts: Vec::new(),
        derives_from_wt0,
        fix: vec![
            "derive the forwarded local port from $WT0_PORT_BASE in this config file".to_owned(),
        ],
    })
}

fn dev_tool_plain_scripts(root: &Path) -> Option<DevTool> {
    let text = package_json_scripts_text(root)?;
    let literal_ports = extract_flag_ports(&text);
    let mentions_dev_tool = ["next dev", "vite", "wrangler dev"]
        .iter()
        .any(|needle| text.contains(needle));
    if literal_ports.is_empty() && !mentions_dev_tool {
        return None;
    }
    let derives_from_wt0 = text.contains("WT0_PORT_BASE") || text.contains("WT0_SLUG");
    Some(DevTool {
        tool: "plain dev script (package.json)".to_owned(),
        files: vec![root.join("package.json")],
        literal_ports,
        literal_hosts: Vec::new(),
        derives_from_wt0,
        fix: vec![
            "pass -p $WT0_PORT_BASE or PORT=$WT0_PORT_BASE instead of a literal port".to_owned(),
            "e.g. `next dev -p $WT0_PORT_BASE` — wt0 init dev writes a .env.wt0 to source"
                .to_owned(),
        ],
    })
}

fn package_json_scripts_text(root: &Path) -> Option<String> {
    let value = package_json(root)?;
    let scripts = value.get("scripts")?.as_object()?;
    let mut combined = String::new();
    for (name, script) in scripts {
        if let Some(text) = script.as_str() {
            combined.push_str(name);
            combined.push(' ');
            combined.push_str(text);
            combined.push('\n');
        }
    }
    Some(combined)
}

fn concat_files(paths: &[PathBuf]) -> String {
    let mut combined = String::new();
    for path in paths {
        if let Ok(text) = fs::read_to_string(path) {
            combined.push('\n');
            combined.push_str(&text);
        }
    }
    combined
}

/// Literal port numbers following a `--port`, `-p`, or `PORT=` marker —
/// covers Procfile/mprocs/concurrently command lines and plain `package.json`
/// dev scripts (`next dev -p 3000`, `vite --port 3000`, `PORT=3000 node …`,
/// `wrangler dev --port 3000`). Deliberately approximate, like
/// `extract_literal_ports` above: informational only, never a refusal.
fn extract_flag_ports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for marker in ["--port=", "--port ", "-p ", "PORT="] {
        let mut search_from = 0usize;
        while let Some(relative) = text[search_from..].find(marker) {
            let marker_end = search_from + relative + marker.len();
            let after = text[marker_end..].trim_start_matches(' ');
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if (2..=5).contains(&digits.len()) && !found.iter().any(|existing| existing == &digits)
            {
                found.push(digits.clone());
            }
            search_from = marker_end;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_literal_ports_only_from_port_mentioning_lines() {
        let text = "TILT_PORT=\"${TILT_PORT:-10380}\"\nVERSION=2026\nk8s_resource('web', port_forwards='3000:3000')\n";
        let ports = extract_literal_ports(text);
        assert!(ports.iter().any(|port| port == "10380"), "{ports:?}");
        assert!(ports.iter().any(|port| port == "3000"), "{ports:?}");
        assert!(!ports.iter().any(|port| port == "2026"), "{ports:?}");
    }

    #[test]
    fn extracts_localhost_hostname_tokens() {
        let text = "url = 'http://web-flam.localhost:1355'\nother line\n";
        let hosts = extract_tokens_containing(text, ".localhost");
        assert!(
            hosts.iter().any(|host| host.contains("web-flam.localhost")),
            "{hosts:?}"
        );
    }

    fn fixture_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wt0-tooling-tilt-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    /// A Tiltfile with a hard-coded port and no reference to wt0's own
    /// per-runtime identity: `wt0 doctor` must call this out as a collision
    /// risk, matching the pinned-port Tiltfile the "the Tilt line" spec
    /// describes.
    #[test]
    fn detect_tilt_flags_a_literal_port_with_no_wt0_reference() {
        let root = fixture_dir("literal");
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(
            root.join("Tiltfile"),
            "k8s_resource('web', port_forwards='10350:3000')\nk8s_yaml(read_file('k8s/app.yaml'))\n",
        )
        .expect("write Tiltfile");

        let report = detect_tilt(&root);
        assert!(report.detected);
        assert!(!report.derives_from_wt0, "{:?}", report.literal_ports);
        assert!(
            report.literal_ports.iter().any(|port| port == "10350"),
            "{:?}",
            report.literal_ports
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    /// The same shape, but the Tiltfile derives its port from
    /// `WT0_PORT_BASE` (the pattern FLAM and Builders Stack both use) — no
    /// collision, so `doctor` must say so instead of proposing a fix.
    #[test]
    fn detect_tilt_recognizes_a_tiltfile_that_already_derives_from_wt0() {
        let root = fixture_dir("derived");
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(
            root.join("Tiltfile"),
            "load('ext://wt0', 'wt0_port')\nTILT_PORT = wt0_port(99)\nWT0_SLUG = os.environ.get('WT0_SLUG', '')\n",
        )
        .expect("write Tiltfile");

        let report = detect_tilt(&root);
        assert!(report.detected);
        assert!(report.derives_from_wt0);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn detect_tilt_reports_not_detected_with_no_tiltfile() {
        let root = fixture_dir("absent");
        fs::create_dir_all(&root).expect("create fixture root");

        let report = detect_tilt(&root);
        assert!(!report.detected);
        assert!(!report.derives_from_wt0);
        assert!(report.literal_ports.is_empty());

        fs::remove_dir_all(root).expect("remove fixture");
    }

    // -----------------------------------------------------------------------
    // Not everyone uses Tilt: docker-compose, devcontainer, Procfile, plain
    // dev scripts — and one repository already deriving from wt0.
    // -----------------------------------------------------------------------

    /// A docker-compose file with literal host ports and a `container_name`,
    /// no reference to wt0's identity — real shape from a design partner's
    /// `docker-compose.yml` (a Postgres service pinned to host port 5432,
    /// named `delulus-postgres`).
    #[test]
    fn detect_dev_environment_flags_compose_literal_ports_and_container_names() {
        let root = fixture_dir("compose-literal");
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(
            root.join("docker-compose.yml"),
            "services:\n  \
             postgres:\n    \
             image: postgres:16-alpine\n    \
             container_name: delulus-postgres\n    \
             ports:\n      \
             - \"5432:5432\"\n",
        )
        .expect("write compose file");

        let tools = detect_dev_environment(&root);
        let compose = tools
            .iter()
            .find(|tool| tool.tool == "docker-compose")
            .expect("docker-compose detected");
        assert!(!compose.derives_from_wt0);
        assert!(
            compose.literal_ports.iter().any(|port| port == "5432"),
            "{:?}",
            compose.literal_ports
        );
        assert!(
            compose
                .literal_hosts
                .iter()
                .any(|host| host == "delulus-postgres"),
            "{:?}",
            compose.literal_hosts
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn compose_service_ports_associates_ports_with_their_own_service() {
        let text = "services:\n  \
                     web:\n    \
                     ports:\n      \
                     - \"3000:3000\"\n  \
                     redis:\n    \
                     ports:\n      \
                     - \"${REDIS_PORT:-6379}:6379\"\n";
        let services = compose_service_ports(text);
        assert_eq!(
            services
                .iter()
                .find(|(name, _)| name == "web")
                .map(|(_, ports)| ports.as_slice()),
            Some(&[("3000".to_owned(), "3000".to_owned())][..])
        );
        assert_eq!(
            services
                .iter()
                .find(|(name, _)| name == "redis")
                .map(|(_, ports)| ports.as_slice()),
            Some(&[("6379".to_owned(), "6379".to_owned())][..])
        );
    }

    /// `.devcontainer/devcontainer.json` with a literal `forwardPorts` entry
    /// and `//` comments (JSONC, as VS Code writes it) — must still parse.
    #[test]
    fn detect_dev_environment_flags_devcontainer_forward_ports() {
        let root = fixture_dir("devcontainer");
        fs::create_dir_all(root.join(".devcontainer")).expect("create .devcontainer");
        fs::write(
            root.join(".devcontainer/devcontainer.json"),
            "// see https://aka.ms/devcontainer.json\n\
             {\n  \
             \"name\": \"Node.js\",\n  \
             // ports the container listens on\n  \
             \"forwardPorts\": [10350]\n\
             }\n",
        )
        .expect("write devcontainer.json");

        let tools = detect_dev_environment(&root);
        let devcontainer = tools
            .iter()
            .find(|tool| tool.tool == "devcontainer")
            .expect("devcontainer detected");
        assert!(!devcontainer.derives_from_wt0);
        assert!(
            devcontainer
                .literal_ports
                .iter()
                .any(|port| port == "10350"),
            "{:?}",
            devcontainer.literal_ports
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    /// A Procfile with a literal `--port` — the fix is generic (`wt0 init
    /// dev`), not tool-specific, since Procfile/overmind/foreman/mprocs all
    /// just execute the command line as written.
    #[test]
    fn detect_dev_environment_flags_procfile_literal_port() {
        let root = fixture_dir("procfile");
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(root.join("Procfile"), "web: node server.js --port 3000\n")
            .expect("write Procfile");

        let tools = detect_dev_environment(&root);
        let procfile = tools
            .iter()
            .find(|tool| tool.tool == "Procfile / process manager")
            .expect("Procfile detected");
        assert!(!procfile.derives_from_wt0);
        assert!(
            procfile.literal_ports.iter().any(|port| port == "3000"),
            "{:?}",
            procfile.literal_ports
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    /// `package.json`'s own `dev` script with a literal `-p 3000` — the
    /// "not everyone uses Tilt" baseline case: a plain framework dev server.
    #[test]
    fn detect_dev_environment_flags_plain_dev_script_literal_port() {
        let root = fixture_dir("plain-dev-script");
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(
            root.join("package.json"),
            "{\"scripts\": {\"dev\": \"next dev -p 3000\"}}\n",
        )
        .expect("write package.json");

        let tools = detect_dev_environment(&root);
        let plain = tools
            .iter()
            .find(|tool| tool.tool == "plain dev script (package.json)")
            .expect("plain dev script detected");
        assert!(!plain.derives_from_wt0);
        assert!(
            plain.literal_ports.iter().any(|port| port == "3000"),
            "{:?}",
            plain.literal_ports
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    /// A repository whose dev script already reads `$WT0_PORT_BASE`: no
    /// literal port to fix, and `derives_from_wt0` must be true so `wt0
    /// doctor` reports it collision-free instead of proposing `wt0 init dev`
    /// again.
    #[test]
    fn detect_dev_environment_recognizes_a_script_that_already_derives_from_wt0() {
        let root = fixture_dir("derives");
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(
            root.join("package.json"),
            "{\"scripts\": {\"dev\": \"next dev -p $WT0_PORT_BASE\"}}\n",
        )
        .expect("write package.json");

        let tools = detect_dev_environment(&root);
        let plain = tools
            .iter()
            .find(|tool| tool.tool == "plain dev script (package.json)")
            .expect("plain dev script detected");
        assert!(plain.derives_from_wt0);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn extract_flag_ports_reads_dash_p_port_equals_and_double_dash_port() {
        let text = "next dev -p 3000\nvite --port 4000\nPORT=5000 node server.js\nwrangler dev --port=6000\n";
        let ports = extract_flag_ports(text);
        for expected in ["3000", "4000", "5000", "6000"] {
            assert!(ports.iter().any(|port| port == expected), "{ports:?}");
        }
    }
}
