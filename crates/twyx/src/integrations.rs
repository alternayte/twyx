//! `twyx integration`: install, show and remove the hooks or plugin that
//! let an agent report its state and session.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use similar::TextDiff;
use twyx_core::agent::{AgentKind, AgentKinds};
use twyx_core::integration::{Change, Integration, Locations, VERSION};
use twyx_core::paths::Paths;

use crate::projects::ask;

#[derive(Debug, Subcommand)]
pub(crate) enum IntegrationCommand {
    /// Show the change to the agent's config, and write it after you confirm.
    Install {
        /// The agent kinds, such as claude, pi or opencode.
        #[arg(required = true)]
        agents: Vec<String>,
        /// Write without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Show which integrations are installed, and their version.
    Status,
    /// Show the change that removes the integration, and write it after you
    /// confirm.
    Remove {
        #[arg(required = true)]
        agents: Vec<String>,
        /// Write without asking.
        #[arg(long)]
        yes: bool,
    },
}

pub(crate) fn integration(command: IntegrationCommand) -> Result<()> {
    let kinds = AgentKinds::load(&Paths::discover()?.config.join("agents"))?;
    let at = Locations::from_env()?;
    match command {
        IntegrationCommand::Install { agents, yes } => {
            let mut changes = Vec::new();
            for name in &agents {
                let (kind, integration) = find(&kinds, name)?;
                match integration.install(&at, &kind.name)? {
                    Some(change) => changes.push(change),
                    None => say!("{name}: version {VERSION} is installed"),
                }
            }
            write(&changes, yes, "Install")
        }
        IntegrationCommand::Status => {
            for kind in kinds.all() {
                let Some(integration) = kind.integration else {
                    continue;
                };
                let file = integration.file(&at);
                let state = match integration.status(&at) {
                    Ok(Some(VERSION)) => format!("version {VERSION} installed"),
                    Ok(Some(old)) => {
                        format!(
                            "version {old} installed; twyx integration install {} updates it to {VERSION}",
                            kind.name
                        )
                    }
                    Ok(None) => "not installed".to_owned(),
                    Err(error) => format!("{error:#}"),
                };
                say!("{}  {state}  ({})", kind.name, file.display());
            }
            Ok(())
        }
        IntegrationCommand::Remove { agents, yes } => {
            let mut changes = Vec::new();
            for name in &agents {
                let (_, integration) = find(&kinds, name)?;
                match integration.remove(&at)? {
                    Some(change) => changes.push(change),
                    None => say!("{name}: not installed"),
                }
            }
            write(&changes, yes, "Write")
        }
    }
}

fn find<'a>(kinds: &'a AgentKinds, name: &str) -> Result<(&'a AgentKind, Integration)> {
    let kind = kinds.get(name).with_context(|| {
        let names: Vec<&str> = kinds.all().map(|kind| kind.name.as_str()).collect();
        format!(
            "{name:?} is not an agent kind; the kinds are {}",
            names.join(", ")
        )
    })?;
    let integration = kind
        .integration
        .with_context(|| format!("{name} has no integration; its state comes from the screen"))?;
    Ok((kind, integration))
}

/// Prints each change as a diff, asks once, and writes them all.
fn write(changes: &[Change], yes: bool, verb: &str) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    for change in changes {
        say!("{}: {}", change.path.display(), change.summary);
        let path = change.path.display().to_string();
        let before = change.before.as_deref().unwrap_or("");
        let after = change.after.as_deref().unwrap_or("");
        let diff = TextDiff::from_lines(before, after);
        let (old, new) = match (&change.before, &change.after) {
            (None, _) => ("/dev/null".to_owned(), path),
            (_, None) => (path, "/dev/null".to_owned()),
            _ => (path.clone(), path),
        };
        print!(
            "{}",
            diff.unified_diff().context_radius(3).header(&old, &new)
        );
    }
    let question = format!(
        "{verb} {}?",
        if changes.len() == 1 {
            "this change"
        } else {
            "these changes"
        }
    );
    if !yes
        && !ask(
            &question,
            "nothing written: pass --yes to write without a terminal to ask on",
        )?
    {
        bail!("nothing written");
    }
    for change in changes {
        change.apply()?;
        say!("wrote {}", change.path.display());
    }
    Ok(())
}
