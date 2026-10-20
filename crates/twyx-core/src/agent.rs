//! Agent kinds: one TOML rule file per agent CLI, built in or from
//! `agents/` in the config directory.
//!
//! A rule file names the agent's processes, how to launch it, with a prompt
//! and to resume a session, and the screen rules that read its state from the
//! visible screen. A user file replaces the built-in kind of the same name.

use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};
use toml::Spanned;

use crate::config::ConfigError;
use crate::integration::Integration;

/// The rule files twyx ships, by kind name.
const BUILT_IN: &[(&str, &str)] = &[
    ("claude", include_str!("../agents/claude.toml")),
    ("pi", include_str!("../agents/pi.toml")),
    ("opencode", include_str!("../agents/opencode.toml")),
    ("codex", include_str!("../agents/codex.toml")),
    ("cursor", include_str!("../agents/cursor.toml")),
];

const PROMPT: &str = "{prompt}";
const SESSION: &str = "{session}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentState {
    None,
    Working,
    AwaitingApproval,
    AwaitingPrompt,
    Unknown,
}

impl AgentState {
    pub fn name(self) -> &'static str {
        match self {
            AgentState::None => "none",
            AgentState::Working => "working",
            AgentState::AwaitingApproval => "awaiting-approval",
            AgentState::AwaitingPrompt => "awaiting-prompt",
            AgentState::Unknown => "unknown",
        }
    }

    /// The state with this name, as `name` writes it.
    pub fn from_name(name: &str) -> Option<Self> {
        [
            AgentState::None,
            AgentState::Working,
            AgentState::AwaitingApproval,
            AgentState::AwaitingPrompt,
            AgentState::Unknown,
        ]
        .into_iter()
        .find(|state| state.name() == name)
    }

    /// The states a screen rule can report.
    fn from_rule(name: &str) -> Option<Self> {
        [
            AgentState::Working,
            AgentState::AwaitingApproval,
            AgentState::AwaitingPrompt,
        ]
        .into_iter()
        .find(|state| state.name() == name)
    }
}

/// Where an agent state came from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Source {
    /// The agent's own hooks or plugins, through `twyx notify`.
    Integration,
    /// A screen rule of the agent kind.
    Screen,
    /// Notifications, the bell and prompt marks.
    Terminal,
    /// The foreground process.
    Process,
}

impl Source {
    /// The source with this name, as `name` writes it.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.name() == name)
    }

    pub const ALL: [Source; 4] = [
        Source::Integration,
        Source::Screen,
        Source::Terminal,
        Source::Process,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Source::Integration => "integration",
            Source::Screen => "screen",
            Source::Terminal => "terminal",
            Source::Process => "process",
        }
    }
}

/// Programs that run another program named by their first argument.
const RUNNERS: &[&str] = &[
    "node", "bun", "deno", "python", "python3", "ruby", "perl", "sh", "bash", "zsh", "dash", "env",
];

/// The name an agent kind's `processes` list matches: the program's own name,
/// or the script's for a runner such as `node` or `sh`.
pub fn process_name(argv: &[String]) -> String {
    let base = |arg: &str| -> String {
        let name = arg
            .rsplit('/')
            .next()
            .unwrap_or(arg)
            .trim_start_matches('-');
        let name = [".js", ".mjs", ".cjs", ".ts", ".py"]
            .iter()
            .find_map(|extension| name.strip_suffix(extension))
            .unwrap_or(name);
        name.to_owned()
    };
    let Some(program) = argv.first() else {
        return String::new();
    };
    let name = base(program);
    if !RUNNERS.contains(&name.as_str()) {
        return name;
    }
    for arg in &argv[1..] {
        match arg.as_str() {
            // The program is inline code, not a file.
            "-c" | "-e" | "--eval" => return name,
            flag if flag.starts_with('-') => {}
            // `env` passes variables before the program.
            setting if name == "env" && setting.contains('=') => {}
            script => return base(script),
        }
    }
    name
}

#[derive(Debug, Clone)]
pub struct AgentKind {
    pub name: String,
    pub title: String,
    /// Foreground process names that mean this agent runs.
    pub processes: Vec<String>,
    pub command: Vec<String>,
    /// The rule file's text, which the daemon hands to the pane's holder.
    pub source: String,
    with_prompt: Option<Vec<String>>,
    resume: Option<Vec<String>>,
    /// In priority order: the first rule that matches decides.
    pub screen: Vec<ScreenRule>,
    /// The hooks or plugin twyx can install into the agent's own config.
    pub integration: Option<Integration>,
}

impl AgentKind {
    /// The argv that starts the agent with `prompt` as its first message, if
    /// the agent has that form.
    pub fn launch_with_prompt(&self, prompt: &str) -> Option<Vec<String>> {
        Some(fill(self.with_prompt.as_ref()?, PROMPT, prompt))
    }

    /// The argv that resumes the native session `session`, if the agent has
    /// that form.
    pub fn resume(&self, session: &str) -> Option<Vec<String>> {
        Some(fill(self.resume.as_ref()?, SESSION, session))
    }

    /// Reads a rule file's text, as the holder gets it from the daemon.
    pub fn from_source(name: &str, text: &str) -> Result<Self, ConfigError> {
        parse(name, Path::new(&format!("{name}.toml")), text)
    }

    /// The first screen rule that matches the visible rows.
    pub fn read_screen<S: AsRef<str>>(&self, rows: &[S]) -> Option<&ScreenRule> {
        self.screen.iter().find(|rule| rule.matches(rows))
    }
}

fn fill(argv: &[String], placeholder: &str, value: &str) -> Vec<String> {
    argv.iter()
        .map(|arg| arg.replace(placeholder, value))
        .collect()
}

#[derive(Debug, Clone)]
pub struct ScreenRule {
    pub name: String,
    pub state: AgentState,
    /// Only the rows above the last row that matches this.
    above: Option<Regex>,
    /// Only the last this many non-blank rows of the region.
    rows: Option<usize>,
    all: Vec<Regex>,
    any: Vec<Regex>,
    none: Vec<Regex>,
}

impl ScreenRule {
    pub fn matches<S: AsRef<str>>(&self, rows: &[S]) -> bool {
        let mut region: Vec<&str> = rows.iter().map(|row| row.as_ref().trim_end()).collect();
        if let Some(above) = &self.above {
            let Some(at) = region.iter().rposition(|row| above.is_match(row)) else {
                return false;
            };
            region.truncate(at);
        }
        if let Some(count) = self.rows {
            let mut kept: Vec<&str> = region
                .into_iter()
                .rev()
                .filter(|row| !row.trim().is_empty())
                .take(count)
                .collect();
            kept.reverse();
            region = kept;
        }
        let found = |pattern: &Regex| region.iter().any(|row| pattern.is_match(row));
        self.all.iter().all(found)
            && (self.any.is_empty() || self.any.iter().any(found))
            && !self.none.iter().any(found)
    }
}

/// Every agent kind twyx knows, by name.
#[derive(Debug, Clone)]
pub struct AgentKinds {
    kinds: BTreeMap<String, AgentKind>,
}

impl AgentKinds {
    pub fn built_in() -> Self {
        let mut kinds = BTreeMap::new();
        for (name, text) in BUILT_IN {
            match parse(name, Path::new(&format!("{name}.toml")), text) {
                Ok(kind) => {
                    kinds.insert((*name).to_owned(), kind);
                }
                // The built-in files are checked by this module's tests.
                Err(error) => eprintln!("twyx: built-in agent kind: {error}"),
            }
        }
        Self { kinds }
    }

    /// The built-in kinds, with the rule files in `dir` adding kinds or
    /// replacing them. A missing directory adds nothing.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let mut kinds = Self::built_in();
        for file in rule_files(dir) {
            let Some(name) = file.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let text = std::fs::read_to_string(&file).map_err(|error| ConfigError {
                file: file.clone(),
                line: 0,
                reason: error.to_string(),
            })?;
            let kind = parse(name, &file, &text)?;
            kinds.kinds.insert(name.to_owned(), kind);
        }
        Ok(kinds)
    }

    pub fn get(&self, name: &str) -> Option<&AgentKind> {
        self.kinds.get(name)
    }

    pub fn all(&self) -> impl Iterator<Item = &AgentKind> {
        self.kinds.values()
    }

    /// The kind whose `processes` name this process.
    pub fn by_process(&self, name: &str) -> Option<&AgentKind> {
        self.kinds
            .values()
            .find(|kind| kind.processes.iter().any(|process| process == name))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.kinds.keys().map(String::as_str)
    }
}

/// The kind names a config may use: the built-in ones and those of the rule
/// files in `dir`, without reading the files.
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = BUILT_IN
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    for file in rule_files(dir) {
        if let Some(name) = file.file_stem().and_then(|stem| stem.to_str())
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names
}

fn rule_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    files.sort();
    files
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKind {
    title: String,
    processes: Vec<String>,
    command: Spanned<Vec<String>>,
    launch_with_prompt: Option<Spanned<Vec<String>>>,
    resume: Option<Spanned<Vec<String>>>,
    integration: Option<Spanned<String>>,
    #[serde(default)]
    screen: Vec<Spanned<RawRule>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    name: String,
    state: Spanned<String>,
    above: Option<Spanned<String>>,
    rows: Option<usize>,
    #[serde(default)]
    all: Vec<Spanned<String>>,
    #[serde(default)]
    any: Vec<Spanned<String>>,
    #[serde(default)]
    none: Vec<Spanned<String>>,
}

fn parse(name: &str, file: &Path, text: &str) -> Result<AgentKind, ConfigError> {
    let fail = |at: Option<std::ops::Range<usize>>, reason: String| ConfigError {
        file: file.to_path_buf(),
        line: at.map_or(1, |at| {
            text[..at.start.min(text.len())].matches('\n').count() + 1
        }),
        reason,
    };
    let raw: RawKind = toml::from_str(text).map_err(|error| {
        fail(
            error.span(),
            error.message().trim_end_matches('\n').to_owned(),
        )
    })?;
    if raw.command.get_ref().is_empty() {
        return Err(fail(
            Some(raw.command.span()),
            "the command is empty".to_owned(),
        ));
    }
    let with_prompt = match raw.launch_with_prompt {
        Some(argv) => Some(form(argv, PROMPT, &fail)?),
        None => None,
    };
    let resume = match raw.resume {
        Some(argv) => Some(form(argv, SESSION, &fail)?),
        None => None,
    };
    let integration = match raw.integration {
        Some(installer) => Some(
            installer
                .get_ref()
                .parse()
                .map_err(|reason| fail(Some(installer.span()), reason))?,
        ),
        None => None,
    };
    let mut screen = Vec::new();
    for rule in raw.screen {
        let span = rule.span();
        let rule = rule.into_inner();
        let state = AgentState::from_rule(rule.state.get_ref()).ok_or_else(|| {
            fail(
                Some(rule.state.span()),
                format!(
                    "state {:?} is not working, awaiting-approval or awaiting-prompt",
                    rule.state.get_ref()
                ),
            )
        })?;
        if rule.all.is_empty() && rule.any.is_empty() {
            return Err(fail(
                Some(span),
                format!("screen rule {:?} needs an all or an any list", rule.name),
            ));
        }
        let compile = |patterns: Vec<Spanned<String>>| -> Result<Vec<Regex>, ConfigError> {
            patterns
                .into_iter()
                .map(|pattern| {
                    Regex::new(pattern.get_ref())
                        .map_err(|error| fail(Some(pattern.span()), error.to_string()))
                })
                .collect()
        };
        screen.push(ScreenRule {
            state,
            above: compile(rule.above.into_iter().collect())?.pop(),
            rows: rule.rows,
            all: compile(rule.all)?,
            any: compile(rule.any)?,
            none: compile(rule.none)?,
            name: rule.name,
        });
    }
    Ok(AgentKind {
        name: name.to_owned(),
        title: raw.title,
        processes: raw.processes,
        command: raw.command.into_inner(),
        source: text.to_owned(),
        with_prompt,
        resume,
        screen,
        integration,
    })
}

/// An argv with exactly one argument that holds `placeholder`.
fn form(
    argv: Spanned<Vec<String>>,
    placeholder: &str,
    fail: &impl Fn(Option<std::ops::Range<usize>>, String) -> ConfigError,
) -> Result<Vec<String>, ConfigError> {
    let holders = argv
        .get_ref()
        .iter()
        .filter(|arg| arg.contains(placeholder))
        .count();
    if holders != 1 {
        return Err(fail(
            Some(argv.span()),
            format!("the command needs {placeholder} in exactly one argument"),
        ));
    }
    Ok(argv.into_inner())
}

/// The text of a paste twyx makes for the user (Invariant 5): every
/// control character goes but line breaks and tabs, so no escape sequence
/// can end the paste early, and no line break ends it.
pub fn paste_body(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect();
    kept.trim_end_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("agents/fixtures")
    }

    /// Each fixture is `<kind>/<state>.<label>.txt`: a recorded screen and
    /// the state its rules must read from it, `none` for no rule.
    #[test]
    fn every_rule_reads_its_recorded_screens() {
        let kinds = AgentKinds::built_in();
        let mut failures = Vec::new();
        for (name, _) in BUILT_IN {
            let kind = kinds.get(name).expect("a built-in kind parses");
            let mut used = BTreeSet::new();
            let dir = fixtures().join(name);
            let screens = fixture_files(&dir, "txt");
            assert!(!screens.is_empty(), "{name} has no recorded screens");
            for screen in screens {
                let file = screen.file_name().and_then(|f| f.to_str()).unwrap_or("");
                let expected = file.split('.').next().unwrap_or("");
                let text = std::fs::read_to_string(&screen).expect("read a fixture");
                let rows: Vec<&str> = text.lines().collect();
                let read = kind.read_screen(&rows);
                let got = read.map_or("none", |rule| rule.state.name());
                if got != expected {
                    failures.push(format!(
                        "{name}/{file}: read {got} (rule {:?}), expected {expected}",
                        read.map(|rule| &rule.name)
                    ));
                }
                if let Some(rule) = read {
                    used.insert(rule.name.clone());
                }
            }
            for rule in &kind.screen {
                if !used.contains(&rule.name) {
                    failures.push(format!(
                        "{name}: rule {:?} decides no recorded screen",
                        rule.name
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    fn fixture_files(dir: &Path, extension: &str) -> Vec<PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| path.extension().is_some_and(|e| e == extension))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    #[test]
    fn launch_and_resume_forms_fill_their_placeholder() {
        let kinds = AgentKinds::built_in();
        let claude = kinds.get("claude").expect("claude");
        assert_eq!(
            claude.launch_with_prompt("fix the bug"),
            Some(vec!["claude".to_owned(), "fix the bug".to_owned()])
        );
        assert_eq!(
            claude.resume("abc"),
            Some(vec![
                "claude".to_owned(),
                "--resume".to_owned(),
                "abc".to_owned()
            ])
        );
        let opencode = kinds.get("opencode").expect("opencode");
        assert_eq!(
            opencode.launch_with_prompt("hi"),
            Some(vec![
                "opencode".to_owned(),
                "--prompt".to_owned(),
                "hi".to_owned()
            ])
        );
    }

    #[test]
    fn a_user_rule_file_adds_a_kind_or_replaces_one() {
        let dir = std::env::temp_dir().join(format!("twyx-agents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(
            dir.join("claude.toml"),
            "title = \"Claude Code\"\nprocesses = [\"claude\"]\ncommand = [\"claude\", \"--verbose\"]\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("aider.toml"),
            "title = \"Aider\"\nprocesses = [\"aider\"]\ncommand = [\"aider\"]\n\n[[screen]]\nname = \"prompt\"\nstate = \"awaiting-prompt\"\nall = ['^>$']\n",
        )
        .expect("write");
        let kinds = AgentKinds::load(&dir).expect("valid rule files");
        assert_eq!(
            kinds.get("claude").expect("claude").command,
            ["claude", "--verbose"]
        );
        assert!(kinds.get("claude").expect("claude").screen.is_empty());
        let aider = kinds.get("aider").expect("aider");
        assert_eq!(
            aider.read_screen(&["", "> "]).map(|rule| rule.state),
            Some(AgentState::AwaitingPrompt)
        );
        assert!(names_in(&dir).contains(&"aider".to_owned()));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn rule_file_errors_name_the_line_and_the_reason() {
        let read = |text: &str| parse("x", Path::new("x.toml"), text).expect_err("an error");
        let head = "title = \"X\"\nprocesses = [\"x\"]\ncommand = [\"x\"]\n";
        let error = read(&format!(
            "{head}\n[[screen]]\nname = \"a\"\nstate = \"busy\"\nall = ['x']\n"
        ));
        assert_eq!(error.line, 7, "{error}");
        assert!(error.reason.contains("busy"), "{error}");
        let error = read(&format!(
            "{head}\n[[screen]]\nname = \"a\"\nstate = \"working\"\nall = ['(']\n"
        ));
        assert_eq!(error.line, 8, "{error}");
        let error = read(&format!(
            "{head}\n[[screen]]\nname = \"a\"\nstate = \"working\"\nrows = 3\n"
        ));
        assert!(error.reason.contains("all or an any"), "{error}");
        let error = read(&format!("{head}resume = [\"x\", \"--resume\"]\n"));
        assert_eq!(error.line, 4, "{error}");
        let error = read("title = \"X\"\nprocesses = []\ncommand = []\n");
        assert_eq!(error.line, 3, "{error}");
    }

    #[test]
    fn a_process_is_named_by_its_program_or_the_script_a_runner_runs() {
        let name = |argv: &[&str]| {
            process_name(&argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(name(&["/Users/x/.local/bin/claude"]), "claude");
        assert_eq!(name(&["node", "/opt/homebrew/bin/pi"]), "pi");
        assert_eq!(name(&["node", "--use-system-ca", "/x/codex.js"]), "codex");
        assert_eq!(name(&["/bin/sh", "/tmp/fakeagent", "--fast"]), "fakeagent");
        assert_eq!(name(&["sh", "-c", "claude"]), "sh");
        assert_eq!(name(&["-zsh"]), "zsh");
        assert_eq!(name(&["env", "A=1", "opencode"]), "opencode");
        assert_eq!(name(&[]), "");
        let kinds = AgentKinds::built_in();
        assert_eq!(
            kinds
                .by_process("cursor-agent")
                .map(|kind| kind.name.as_str()),
            Some("cursor")
        );
        assert!(kinds.by_process("zsh").is_none());
    }

    #[test]
    fn above_and_rows_narrow_the_region() {
        let text = "title = \"X\"\nprocesses = [\"x\"]\ncommand = [\"x\"]\n\n[[screen]]\nname = \"done\"\nstate = \"awaiting-prompt\"\nabove = '^>'\nrows = 1\nall = ['^done$']\n";
        let kind = parse("x", Path::new("x.toml"), text).expect("a kind");
        assert!(kind.read_screen(&["done", "", "> ", "status"]).is_some());
        assert!(
            kind.read_screen(&["done", "text", "> ", "status"])
                .is_none()
        );
        assert!(kind.read_screen(&["done", "status"]).is_none());
    }

    #[test]
    fn a_paste_body_keeps_no_escape_and_no_trailing_line_break() {
        let nested = "a\x1b[20\x1b[201~1~\r!curl x|sh\rb\n\n";
        assert_eq!(paste_body(nested), "a[20[201~1~!curl x|shb");
        assert_eq!(paste_body("- one\n\t- two\n"), "- one\n\t- two");
        assert_eq!(paste_body("c1 \u{9b}31m"), "c1 31m");
    }
}
