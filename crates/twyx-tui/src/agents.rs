//! Agent offers in the TUI: Resume for ended agent panes, and the agent
//! integrations, on first run and from the palette.

use std::collections::HashMap;

use twyx_core::agent::AgentKinds;
use twyx_core::integration::{self, Locations};
use twyx_core::paths::Paths;
use twyx_protocol::client::{PaneInfo, WorkspaceInfo};

use twyx_protocol::dialog::Dialog;

use crate::overlay::{Overlay, Purpose};

/// The file whose presence says the first-run offer was made.
fn offered(paths: &Paths) -> std::path::PathBuf {
    paths.state.join("integrations-offered")
}

/// The confirmation that resumes the focused pane's agent session, or all
/// of this workspace's, with each listed.
pub fn resume_offer(
    focused: Option<&PaneInfo>,
    workspace: Option<&WorkspaceInfo>,
    panes: &HashMap<u64, PaneInfo>,
) -> Result<Overlay, String> {
    let Some((focused, kind)) =
        focused.and_then(|pane| pane.resume.as_ref().map(|kind| (pane, kind)))
    else {
        return Err("this pane has no agent session to resume".to_owned());
    };
    let mut all = vec![focused.pane];
    for tab in workspace
        .map(|workspace| workspace.tabs.as_slice())
        .unwrap_or(&[])
    {
        for pane in tab.layout.panes() {
            let pane = pane.raw();
            if pane != focused.pane && panes.get(&pane).is_some_and(|info| info.resume.is_some()) {
                all.push(pane);
            }
        }
    }
    let line = |pane: &PaneInfo, kind: &str| format!("{kind} in {} (pane {})", pane.cwd, pane.pane);
    let mut lines = vec![line(focused, kind)];
    lines.extend(all.iter().skip(1).filter_map(|pane| {
        let info = panes.get(pane)?;
        Some(line(info, info.resume.as_deref()?))
    }));
    let mut keys = vec![('y', "resume this pane".to_owned())];
    if all.len() > 1 {
        keys.push(('a', format!("resume all {}", all.len())));
    }
    Ok(Overlay::Dialog(Dialog::Confirm {
        title: "resume agent sessions".to_owned(),
        lines,
        keys,
        purpose: Purpose::Resume {
            this: focused.pane,
            all,
        },
    }))
}

/// The integrations offer for the agents on `PATH` that lack one, once:
/// the first time any client asks.
pub fn first_run(paths: &Paths) -> Option<Overlay> {
    let marker = offered(paths);
    if marker.exists() {
        return None;
    }
    let offer = integrations_offer(paths).ok();
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "");
    offer
}

/// One confirmation that lists every file change the integrations make.
pub fn integrations_offer(paths: &Paths) -> Result<Overlay, String> {
    let kinds =
        AgentKinds::load(&paths.config.join("agents")).map_err(|error| error.to_string())?;
    let at = Locations::from_env().map_err(|error| format!("{error:#}"))?;
    let offers = integration::offers(&kinds, &at);
    if offers.is_empty() {
        return Err("the agents on PATH have their integrations".to_owned());
    }
    let names: Vec<&str> = offers.iter().map(|(name, ..)| name.as_str()).collect();
    let mut lines = vec![format!(
        "twyx found {} on PATH. An integration reports exact state and the session to resume.",
        names.join(", ")
    )];
    for (name, _, change) in &offers {
        lines.push(match change {
            Ok(change) => format!(
                "{}: {}",
                home(&change.path.display().to_string()),
                change.summary
            ),
            Err(error) => format!("{name}: cannot install: {error:#}"),
        });
    }
    lines.push("twyx integration install shows each change in full.".to_owned());
    lines.push(
        "Without them, state comes from the screen. The palette offers this again.".to_owned(),
    );
    Ok(Overlay::Dialog(Dialog::Confirm {
        title: "agent integrations".to_owned(),
        lines,
        keys: vec![('y', "install".to_owned())],
        purpose: Purpose::Integrations,
    }))
}

/// Installs every offered integration; the outcome, for the status bar.
pub fn install(paths: &Paths) -> String {
    let kinds = match AgentKinds::load(&paths.config.join("agents")) {
        Ok(kinds) => kinds,
        Err(error) => return error.to_string(),
    };
    let at = match Locations::from_env() {
        Ok(at) => at,
        Err(error) => return format!("{error:#}"),
    };
    let mut installed = Vec::new();
    let mut failed = Vec::new();
    for (name, _, change) in integration::offers(&kinds, &at) {
        match change.and_then(|change| change.apply()) {
            Ok(()) => installed.push(name),
            Err(error) => failed.push(format!("{name}: {error:#}")),
        }
    }
    match (installed.is_empty(), failed.first()) {
        (_, Some(error)) => error.clone(),
        (true, None) => "nothing to install".to_owned(),
        (false, None) => format!(
            "installed the {} integrations; agents started from now on use them",
            installed.join(", ")
        ),
    }
}

fn home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}
