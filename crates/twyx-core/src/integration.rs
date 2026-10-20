//! Integrations: the hooks or plugin twyx installs into an agent's own
//! config, so the agent reports its state and session through `twyx notify`.
//!
//! Each change is planned first, as the file's text before and after, so the
//! user sees the exact change before twyx writes it. A marker with the
//! integration version identifies twyx's part, for `status` and `remove`.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::agent::AgentKinds;

/// The version of what twyx writes. A newer one replaces an older one on
/// install.
pub const VERSION: u32 = 1;

const MARK: &str = "twyx integration ";

/// Claude Code's hook events and what each reports.
const CLAUDE_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session"),
    ("UserPromptSubmit", "working"),
    ("PermissionRequest", "awaiting-approval"),
    ("PostToolUse", "working"),
    ("PostToolUseFailure", "working"),
    ("Stop", "turn-finished"),
    ("StopFailure", "error"),
];

const PI: &str = include_str!("../integrations/pi.ts");
const OPENCODE: &str = include_str!("../integrations/opencode.ts");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integration {
    /// Hooks in Claude Code's `settings.json`.
    ClaudeCode,
    /// An extension file in Pi's `extensions/`.
    Pi,
    /// A plugin file in OpenCode's `plugins/`.
    OpenCode,
}

impl FromStr for Integration {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "claude-code" => Ok(Self::ClaudeCode),
            "pi" => Ok(Self::Pi),
            "opencode" => Ok(Self::OpenCode),
            other => Err(format!(
                "integration {other:?} is not claude-code, pi or opencode"
            )),
        }
    }
}

/// Where each agent keeps its config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    pub claude: PathBuf,
    pub pi: PathBuf,
    pub opencode: PathBuf,
}

impl Locations {
    /// The agents' own defaults and the variables that move them.
    pub fn from_env() -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let mut at = Self::under(&home);
        if let Some(dir) = var("CLAUDE_CONFIG_DIR") {
            at.claude = dir.into();
        }
        if let Some(dir) = var("PI_CODING_AGENT_DIR") {
            at.pi = dir.into();
        }
        if let Some(dir) = var("XDG_CONFIG_HOME") {
            at.opencode = PathBuf::from(dir).join("opencode");
        }
        Ok(at)
    }

    /// The defaults under one home directory.
    pub fn under(home: &Path) -> Self {
        Self {
            claude: home.join(".claude"),
            pi: home.join(".pi/agent"),
            opencode: home.join(".config/opencode"),
        }
    }
}

/// One file's text before and after a change; `None` is no file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
    /// What the change does, in a few words.
    pub summary: String,
}

impl Change {
    /// Writes the change: a new file through a rename, so a reader never
    /// sees half of it, with the old file's permissions.
    pub fn apply(&self) -> Result<()> {
        let Some(after) = &self.after else {
            return std::fs::remove_file(&self.path)
                .with_context(|| format!("delete {}", self.path.display()));
        };
        let parent = self.path.parent().context("a file has a directory")?;
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let name = self
            .path
            .file_name()
            .context("a file has a name")?
            .to_string_lossy();
        let temporary = parent.join(format!(".{name}.twyx-{}", std::process::id()));
        std::fs::write(&temporary, after)
            .with_context(|| format!("write {}", temporary.display()))?;
        if let Ok(metadata) = std::fs::metadata(&self.path) {
            std::fs::set_permissions(&temporary, metadata.permissions())
                .with_context(|| format!("set the mode of {}", temporary.display()))?;
        }
        std::fs::rename(&temporary, &self.path)
            .with_context(|| format!("write {}", self.path.display()))
    }
}

impl Integration {
    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Pi => "pi",
            Self::OpenCode => "opencode",
        }
    }

    /// The file the integration changes.
    pub fn file(self, at: &Locations) -> PathBuf {
        match self {
            Self::ClaudeCode => at.claude.join("settings.json"),
            Self::Pi => at.pi.join("extensions/twyx.ts"),
            Self::OpenCode => at.opencode.join("plugins/twyx.ts"),
        }
    }

    /// The installed version, if twyx's part is there.
    pub fn status(self, at: &Locations) -> Result<Option<u32>> {
        let path = self.file(at);
        let Some(text) = read(&path)? else {
            return Ok(None);
        };
        Ok(match self {
            Self::ClaudeCode => {
                let mut versions = Vec::new();
                commands(&settings(&path, &text)?, &mut |command| {
                    versions.extend(version(command));
                });
                versions.into_iter().min()
            }
            Self::Pi | Self::OpenCode => text.lines().next().and_then(version),
        })
    }

    /// The change that installs this version for the agent kind `agent`, or
    /// `None` when it is installed already.
    pub fn install(self, at: &Locations, agent: &str) -> Result<Option<Change>> {
        let path = self.file(at);
        let before = read(&path)?;
        let after = match self {
            Self::ClaudeCode => {
                let mut value = match &before {
                    Some(text) => settings(&path, text)?,
                    None => Value::Object(Map::new()),
                };
                strip(&mut value);
                let Some(object) = value.as_object_mut() else {
                    bail!("{} does not hold a JSON object", path.display());
                };
                let hooks = object
                    .entry("hooks")
                    .or_insert_with(|| Value::Object(Map::new()));
                let Some(hooks) = hooks.as_object_mut() else {
                    bail!("\"hooks\" in {} is not an object", path.display());
                };
                for (event, report) in CLAUDE_EVENTS {
                    let groups = hooks
                        .entry(*event)
                        .or_insert_with(|| Value::Array(Vec::new()));
                    let Some(groups) = groups.as_array_mut() else {
                        bail!("{event} in {} is not a list", path.display());
                    };
                    groups.push(json!({
                        "hooks": [{ "type": "command", "command": claude_command(report, agent) }]
                    }));
                }
                pretty(&value)?
            }
            Self::Pi | Self::OpenCode => {
                if let Some(text) = &before
                    && text.lines().next().and_then(version).is_none()
                {
                    bail!(
                        "{} exists and twyx did not write it; move it away first",
                        path.display()
                    );
                }
                let template = if self == Self::Pi { PI } else { OPENCODE };
                template
                    .replace("{agent}", agent)
                    .replace("{version}", &VERSION.to_string())
            }
        };
        if before.as_deref() == Some(after.as_str()) {
            return Ok(None);
        }
        let summary = match (self, before.is_some() && self.status(at)?.is_some()) {
            (Self::ClaudeCode, true) => "updates the hooks that run twyx notify",
            (Self::ClaudeCode, false) => "adds hooks that run twyx notify",
            (Self::Pi, true) => "updates the Pi extension that runs twyx notify",
            (Self::Pi, false) => "adds a Pi extension that runs twyx notify",
            (Self::OpenCode, true) => "updates the OpenCode plugin that runs twyx notify",
            (Self::OpenCode, false) => "adds an OpenCode plugin that runs twyx notify",
        };
        Ok(Some(Change {
            path,
            before,
            after: Some(after),
            summary: summary.to_owned(),
        }))
    }

    /// The change that removes twyx's part, or `None` when it is not there.
    pub fn remove(self, at: &Locations) -> Result<Option<Change>> {
        if self.status(at)?.is_none() {
            return Ok(None);
        }
        let path = self.file(at);
        let before = read(&path)?;
        let (after, summary) = match self {
            Self::ClaudeCode => {
                let mut value = settings(&path, before.as_deref().unwrap_or("{}"))?;
                strip(&mut value);
                (
                    Some(pretty(&value)?),
                    "removes the hooks that run twyx notify",
                )
            }
            Self::Pi => (None, "deletes the Pi extension"),
            Self::OpenCode => (None, "deletes the OpenCode plugin"),
        };
        Ok(Some(Change {
            path,
            before,
            after,
            summary: summary.to_owned(),
        }))
    }
}

/// The agent kinds with an integration whose first command is on `PATH`,
/// and the change that installs each, for the ones not installed at this
/// version.
pub fn offers(kinds: &AgentKinds, at: &Locations) -> Vec<(String, Integration, Result<Change>)> {
    kinds
        .all()
        .filter_map(|kind| {
            let integration = kind.integration?;
            on_path(kind.command.first()?).then_some(())?;
            let change = integration.install(at, &kind.name).transpose()?;
            Some((kind.name.clone(), integration, change))
        })
        .collect()
}

/// Whether `program` names an executable file in a directory on `PATH`.
pub fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(program))
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn settings(path: &Path, text: &str) -> Result<Value> {
    serde_json::from_str(text).with_context(|| format!("read {} as JSON", path.display()))
}

fn pretty(value: &Value) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value).context("write JSON")?;
    text.push('\n');
    Ok(text)
}

/// The hook command. It does nothing outside a twyx pane or without twyx on
/// `PATH`, and it always exits 0: Claude Code reads exit 2 as "block".
fn claude_command(report: &str, agent: &str) -> String {
    format!(
        "[ -n \"$TWYX_PANE_ID\" ] && command -v twyx >/dev/null && \
         twyx notify {report} --agent {agent} --session-from-stdin session_id; \
         exit 0 # {MARK}{VERSION}"
    )
}

/// The version in a line that carries twyx's mark.
fn version(line: &str) -> Option<u32> {
    let (_, after) = line.split_once(MARK)?;
    after
        .split(|ch: char| !ch.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Calls `found` with each hook command in Claude Code settings.
fn commands(value: &Value, found: &mut impl FnMut(&str)) {
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return;
    };
    for groups in hooks.values().filter_map(Value::as_array) {
        for group in groups {
            for hook in group
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(command) = hook.get("command").and_then(Value::as_str) {
                    found(command);
                }
            }
        }
    }
}

/// Removes twyx's hooks from Claude Code settings, and each group, event
/// and `hooks` table that only they filled.
fn strip(value: &mut Value) {
    let ours = |hook: &Value| {
        hook.get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| version(command).is_some())
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(hooks) = object.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    let mut emptied = false;
    hooks.retain(|_, groups| {
        let Some(list) = groups.as_array_mut() else {
            return true;
        };
        let before = list.len();
        list.retain_mut(|group| {
            let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let count = inner.len();
            inner.retain(|hook| !ours(hook));
            !(inner.is_empty() && count > 0)
        });
        let keep = !(list.is_empty() && before > 0);
        emptied |= !keep;
        keep
    });
    if emptied && hooks.is_empty() {
        object.remove("hooks");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "twyx-integration-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn claude_code_hooks_go_in_beside_the_users_own_and_come_out_exactly() {
        let home = home();
        let at = Locations::under(&home);
        let own = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"say done\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";
        std::fs::create_dir_all(&at.claude).unwrap();
        std::fs::write(at.claude.join("settings.json"), own).unwrap();
        assert_eq!(Integration::ClaudeCode.status(&at).unwrap(), None);

        let change = Integration::ClaudeCode
            .install(&at, "claude")
            .unwrap()
            .unwrap();
        change.apply().unwrap();
        assert_eq!(Integration::ClaudeCode.status(&at).unwrap(), Some(VERSION));
        let installed: Value = serde_json::from_str(change.after.as_ref().unwrap()).unwrap();
        assert_eq!(installed["model"], "opus");
        assert_eq!(
            installed["hooks"]["Stop"][0]["hooks"][0]["command"],
            "say done"
        );
        let ours = installed["hooks"]["Stop"][1]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            ours.contains("twyx notify turn-finished --agent claude"),
            "{ours}"
        );
        assert!(
            installed["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("awaiting-approval")
        );
        assert_eq!(
            Integration::ClaudeCode.install(&at, "claude").unwrap(),
            None,
            "a second install changes nothing"
        );

        Integration::ClaudeCode
            .remove(&at)
            .unwrap()
            .unwrap()
            .apply()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(at.claude.join("settings.json")).unwrap(),
            own
        );
        assert_eq!(Integration::ClaudeCode.remove(&at).unwrap(), None);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn an_older_version_is_replaced_and_a_file_twyx_did_not_write_is_left_alone() {
        let home = home();
        let at = Locations::under(&home);
        let old = claude_command("working", "claude")
            .replace(&format!("{MARK}{VERSION}"), &format!("{MARK}0"));
        let settings =
            json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": old }] }] } });
        std::fs::create_dir_all(&at.claude).unwrap();
        std::fs::write(at.claude.join("settings.json"), pretty(&settings).unwrap()).unwrap();
        assert_eq!(Integration::ClaudeCode.status(&at).unwrap(), Some(0));
        let change = Integration::ClaudeCode
            .install(&at, "claude")
            .unwrap()
            .unwrap();
        assert!(!change.after.as_ref().unwrap().contains(&format!("{MARK}0")));

        let plugin = Integration::OpenCode.file(&at);
        std::fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        std::fs::write(&plugin, "export const Mine = async () => ({})\n").unwrap();
        assert_eq!(Integration::OpenCode.status(&at).unwrap(), None);
        assert!(Integration::OpenCode.install(&at, "opencode").is_err());
        assert_eq!(Integration::OpenCode.remove(&at).unwrap(), None);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_plugin_file_is_written_for_its_kind_and_deleted_on_remove() {
        let home = home();
        let at = Locations::under(&home);
        let change = Integration::Pi.install(&at, "pi").unwrap().unwrap();
        assert_eq!(change.before, None);
        let text = change.after.as_ref().unwrap();
        assert!(text.contains("\"--agent\", \"pi\""), "{text}");
        assert!(!text.contains("{agent}") && !text.contains("{version}"));
        change.apply().unwrap();
        assert_eq!(Integration::Pi.status(&at).unwrap(), Some(VERSION));
        let removal = Integration::Pi.remove(&at).unwrap().unwrap();
        assert_eq!(removal.after, None);
        removal.apply().unwrap();
        assert!(!Integration::Pi.file(&at).exists());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
