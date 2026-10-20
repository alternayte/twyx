//! New tasks: a worktree per agent from one start ref, its setup, then the
//! agent with the prompt.
//!
//! The agent starts once the setup pane ends with exit 0, or at once when
//! there is no setup to run. Closing or killing the setup pane skips it.
//! After a failed setup the agent waits; "Run again" carries the wait to
//! the new setup pane.

use twyx_core::pane::PaneId;
use twyx_protocol::client::{FromDaemon, Launch, PaneState};

use crate::daemon::{Daemon, Pane};
use crate::worktrees::Then;

#[derive(Debug, Clone)]
pub(crate) struct Task {
    pub branch: String,
    /// The agent kind; `None` runs a shell.
    pub agent: Option<String>,
    pub prompt: Option<String>,
    /// Whether to show the task's workspace.
    pub switch: bool,
}

/// A task waiting for its setup pane.
#[derive(Debug)]
pub(crate) struct Waiting {
    client: u64,
    workspace: u64,
    task: Task,
}

/// `text` as a branch name part: lower case letters, digits and single
/// dashes.
pub(crate) fn slug(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut slug: String = slug.chars().take(40).collect();
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// A name for a task with none: the prompt's first words, else the time.
fn made_name(prompt: Option<&str>) -> String {
    let words: Vec<&str> = prompt
        .unwrap_or_default()
        .split_whitespace()
        .take(5)
        .collect();
    let name = slug(&words.join(" "));
    if name.is_empty() {
        format!("task-{}", crate::agents::now_ms() / 1000)
    } else {
        name
    }
}

impl Daemon {
    pub(crate) fn new_task(
        &mut self,
        client: u64,
        project: u64,
        name: &str,
        start: Option<String>,
        agents: Vec<Option<String>>,
        prompt: Option<String>,
    ) {
        self.reload_config();
        let agents = if agents.is_empty() {
            vec![self.config.default_agent.clone()]
        } else {
            agents
        };
        if let Some(unknown) = agents
            .iter()
            .flatten()
            .find(|kind| self.agents.get(kind).is_none())
        {
            let kinds: Vec<&str> = self.agents.names().collect();
            self.refuse(
                client,
                format!(
                    "{unknown:?} is not an agent kind; the kinds are {}",
                    kinds.join(", ")
                ),
            );
            return;
        }
        let prompt = prompt.filter(|prompt| !prompt.trim().is_empty());
        let base = match slug(name) {
            named if !named.is_empty() => named,
            _ => made_name(prompt.as_deref()),
        };
        let prefix = self.config.branch_prefix.clone().unwrap_or_default();
        for (number, agent) in agents.into_iter().enumerate() {
            let branch = match number {
                0 => format!("{prefix}{base}"),
                _ => format!("{prefix}{base}-{}", number + 1),
            };
            let task = Task {
                branch: branch.clone(),
                agent,
                prompt: prompt.clone(),
                switch: number == 0,
            };
            self.new_worktree(
                client,
                project,
                branch,
                start.clone(),
                Some(Then::Task(task)),
            );
        }
    }

    /// A task's worktree is set up, or its setup runs in `setup`.
    pub(crate) fn task_ready(
        &mut self,
        client: u64,
        workspace: u64,
        task: Task,
        setup: Option<PaneId>,
    ) {
        match setup {
            Some(pane) => {
                self.tasks.insert(
                    pane,
                    Waiting {
                        client,
                        workspace,
                        task,
                    },
                );
            }
            None => self.start_task(client, workspace, task),
        }
    }

    /// A setup pane ended, or the user closed it.
    pub(crate) fn setup_ended(&mut self, pane: PaneId, state: Option<&PaneState>) {
        let go = match state {
            Some(PaneState::Exited { code }) => *code == 0,
            Some(PaneState::Killed { .. }) | None => true,
            Some(PaneState::Running | PaneState::Lost) => false,
        };
        if go && let Some(waiting) = self.tasks.remove(&pane) {
            self.start_task(waiting.client, waiting.workspace, waiting.task);
        }
    }

    /// The setup pane `old` runs again as `new`; its task waits for it.
    pub(crate) fn setup_moved(&mut self, old: PaneId, new: PaneId) {
        if let Some(waiting) = self.tasks.remove(&old) {
            self.tasks.insert(new, waiting);
        }
    }

    /// Starts the task's agent: in the repository's trusted `default` recipe
    /// when it has an agent pane, else in a tab of its own.
    fn start_task(&mut self, client: u64, workspace: u64, task: Task) {
        let kind = task.agent.as_deref().and_then(|kind| self.agents.get(kind));
        let plain = kind.map_or(Launch::Shell, |kind| Launch::Command(kind.command.clone()));
        let prompted = match (kind, &task.prompt) {
            (Some(kind), Some(prompt)) => kind.launch_with_prompt(prompt).map(Launch::Command),
            _ => None,
        };
        let note = match (&task.prompt, &prompted) {
            (Some(prompt), None) => {
                let target = self.last_used_client();
                if let Some(target) = target.and_then(|id| self.clients.get(&id)) {
                    self.post(
                        &target.out,
                        FromDaemon::Clipboard {
                            pane: 0,
                            text: prompt.clone(),
                        },
                    );
                }
                Some(format!(
                    "{} takes no prompt at start; the prompt is on the clipboard",
                    task.agent.as_deref().unwrap_or("a shell")
                ))
            }
            _ => None,
        };
        let launches = (prompted.unwrap_or_else(|| plain.clone()), plain);
        let recipe = self
            .runnable(workspace, "default")
            .ok()
            .filter(|recipe| recipe.panes.values().any(|pane| pane.agent));
        let started = match recipe {
            Some(recipe) => self
                .recipe_tabs(workspace, &recipe, Some(&launches))
                .map(|_| ()),
            None => self.agent_tab(workspace, &task, launches.0).map(|_| ()),
        };
        if let Err(reason) = started {
            self.refuse(client, reason);
            return;
        }
        if task.switch {
            self.switch_for(client, workspace, true);
        }
        self.broadcast_workspace();
        self.reply(
            client,
            FromDaemon::TaskStarted {
                workspace,
                branch: task.branch,
                note,
            },
        );
    }

    fn agent_tab(&mut self, workspace: u64, task: &Task, launch: Launch) -> Result<PaneId, String> {
        let Some(directory) = self
            .workspaces
            .get(&workspace)
            .map(|entry| entry.directory.clone())
        else {
            return Err(format!("no workspace {workspace}"));
        };
        let record = Pane {
            agent: crate::agents::PaneAgent::default(),
            link: None,
            cols: 80,
            rows: 24,
            state: PaneState::Running,
            screen: None,
            launch,
            spawn_worktree: self.worktree_at(&directory),
            cwd: directory,
            title: String::new(),
            history: self.config.history,
            workspace,
            env: Vec::new(),
        };
        self.command_tab(workspace, task.agent.as_deref().unwrap_or("shell"), record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_names_become_branch_parts() {
        assert_eq!(slug("Fix the Login bug!"), "fix-the-login-bug");
        assert_eq!(slug("  --ünïcode__ok  "), "n-code-ok");
        assert_eq!(
            made_name(Some("Add a retry to the upload client please")),
            "add-a-retry-to-the"
        );
        assert!(made_name(None).starts_with("task-"));
    }
}
