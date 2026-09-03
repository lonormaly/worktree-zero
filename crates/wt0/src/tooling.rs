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
}
