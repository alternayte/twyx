//! Recipes in a workspace: finding them, trusting them, and opening their
//! tabs.
//!
//! A repository recipe runs only while its exact content is trusted
//! (Invariant 13). The daemon hashes the bytes it parses, so what runs is
//! what was trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use twyx_core::layout::{self, Floating, Fractions, Layout, Leaf};
use twyx_core::recipe::{self, Command, Node, Recipe};
use twyx_protocol::client::{
    FromDaemon, Launch, PaneState, RecipeInfo, RecipePaneInfo, RecipeSource,
};

use crate::daemon::{Daemon, Pane, area_cells};

/// Where repository recipes live, inside a workspace's directory.
const REPOSITORY_DIR: &str = ".twyx/recipes";

/// One recipe file, read once: its bytes are hashed and parsed together.
#[derive(Debug)]
struct Found {
    source: Source,
    recipe: Result<Recipe, String>,
}

#[derive(Debug, Clone)]
enum Source {
    BuiltIn,
    User(PathBuf),
    Repository {
        project: u64,
        /// Relative to the workspace directory, as the trust store keys it.
        path: String,
        hash: String,
    },
}

impl Source {
    /// Repository recipes win over the user's, and the user's over the
    /// built-in ones.
    fn rank(&self) -> u8 {
        match self {
            Source::Repository { .. } => 0,
            Source::User(_) => 1,
            Source::BuiltIn => 2,
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The `.toml` files of a directory, by name.
fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    files.sort();
    files
}

fn read(path: &Path) -> (Vec<u8>, Result<Recipe, String>) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return (Vec::new(), Err(format!("{}: {error}", path.display()))),
    };
    let recipe = match std::str::from_utf8(&bytes) {
        Ok(text) => recipe::parse(path, text).map_err(|error| error.to_string()),
        Err(_) => Err(format!("{}: not UTF-8", path.display())),
    };
    (bytes, recipe)
}

/// A pane's directory: relative to the workspace's, under `~/`, or absolute.
fn pane_dir(workspace: &str, dir: Option<&str>) -> PathBuf {
    match dir {
        None => PathBuf::from(workspace),
        Some(dir) => match dir.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map_or_else(|| PathBuf::from(dir), |home| Path::new(&home).join(rest)),
            None => Path::new(workspace).join(dir),
        },
    }
}

impl Daemon {
    /// Every recipe a workspace can run, best source first for each name.
    fn found_recipes(&self, workspace: u64) -> Vec<Found> {
        let mut found: Vec<Found> = recipe::built_in()
            .into_iter()
            .map(|recipe| Found {
                source: Source::BuiltIn,
                recipe: Ok(recipe),
            })
            .collect();
        for path in toml_files(&self.paths.config.join("recipes")) {
            let (_, recipe) = read(&path);
            found.push(Found {
                source: Source::User(path),
                recipe,
            });
        }
        if let Some(entry) = self.workspaces.get(&workspace)
            && let Some(project) = entry.project
        {
            let root = Path::new(&entry.directory);
            for path in toml_files(&root.join(REPOSITORY_DIR)) {
                let (bytes, recipe) = read(&path);
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                found.push(Found {
                    source: Source::Repository {
                        project,
                        path: relative,
                        hash: hash(&bytes),
                    },
                    recipe,
                });
            }
        }
        found.sort_by_key(|found| found.source.rank());
        found
    }

    fn recipe_name(found: &Found) -> String {
        match (&found.recipe, &found.source) {
            (Ok(recipe), _) => recipe.name.clone(),
            (Err(_), Source::User(path)) => stem(path),
            (Err(_), Source::Repository { path, .. }) => stem(Path::new(path)),
            (Err(_), Source::BuiltIn) => String::new(),
        }
    }

    pub(crate) fn list_recipes(&self, client: u64, workspace: u64) {
        let found = self.found_recipes(workspace);
        let mut seen = Vec::new();
        let infos = found
            .iter()
            .map(|found| {
                let name = Self::recipe_name(found);
                let shadowed = seen.contains(&name);
                seen.push(name.clone());
                let source = match &found.source {
                    Source::BuiltIn => RecipeSource::BuiltIn,
                    Source::User(path) => RecipeSource::User {
                        path: path.to_string_lossy().into_owned(),
                    },
                    Source::Repository {
                        project,
                        path,
                        hash,
                    } => RecipeSource::Repository {
                        path: path.clone(),
                        hash: hash.clone(),
                        trusted: self.store.trusted(*project, path, hash).unwrap_or(false),
                    },
                };
                let (description, tabs, panes, new_worktree, error) = match &found.recipe {
                    Ok(recipe) => (
                        recipe.description.clone(),
                        recipe.tabs.len() as u32,
                        recipe
                            .panes
                            .iter()
                            .map(|(name, pane)| RecipePaneInfo {
                                name: name.clone(),
                                command: pane.command.as_ref().map(Command::display),
                                dir: pane.dir.clone(),
                                env: pane
                                    .env
                                    .iter()
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                                agent: pane.agent,
                            })
                            .collect(),
                        recipe.worktree.is_some(),
                        None,
                    ),
                    Err(error) => (String::new(), 0, Vec::new(), false, Some(error.clone())),
                };
                RecipeInfo {
                    name,
                    description,
                    source,
                    error,
                    tabs,
                    panes,
                    new_worktree,
                    shadowed,
                }
            })
            .collect();
        self.reply(client, FromDaemon::Recipes(infos));
    }

    /// The recipe a name runs in a workspace, refused when it is a
    /// repository recipe whose content is not trusted (Invariant 13).
    fn runnable(&self, workspace: u64, name: &str) -> Result<Recipe, String> {
        let found = self
            .found_recipes(workspace)
            .into_iter()
            .find(|found| Self::recipe_name(found) == name)
            .ok_or_else(|| format!("no recipe {name}; twyx recipe list lists them"))?;
        let recipe = found.recipe?;
        if let Source::Repository {
            project,
            path,
            hash,
        } = &found.source
            && !self
                .store
                .trusted(*project, path, hash)
                .map_err(|error| format!("{error:#}"))?
        {
            return Err(format!(
                "recipe {name} ({path}) is not trusted in this content; twyx recipe trust {name} shows it"
            ));
        }
        Ok(recipe)
    }

    pub(crate) fn trust_recipe(
        &mut self,
        client: u64,
        workspace: u64,
        name: String,
        shown: String,
    ) {
        let found = self
            .found_recipes(workspace)
            .into_iter()
            .find(|found| Self::recipe_name(found) == name);
        let Some(Found {
            source:
                Source::Repository {
                    project,
                    path,
                    hash,
                },
            ..
        }) = found
        else {
            self.refuse(client, format!("{name} is not a repository recipe here"));
            return;
        };
        if hash != shown {
            self.refuse(
                client,
                format!("{path} changed since it was shown; look at it again"),
            );
            return;
        }
        match self.store.trust(project, &path, &hash) {
            Ok(()) => self.reply(client, FromDaemon::RecipeTrusted { name }),
            Err(error) => self.refuse(client, format!("{error:#}")),
        }
    }

    pub(crate) fn run_recipe(
        &mut self,
        client: u64,
        workspace: u64,
        name: String,
        branch: Option<String>,
        switch: bool,
    ) {
        let recipe = match self.runnable(workspace, &name) {
            Ok(recipe) => recipe,
            Err(reason) => {
                self.refuse(client, reason);
                return;
            }
        };
        if recipe.worktree.is_some() || branch.is_some() {
            let Some(branch) = branch else {
                self.refuse(
                    client,
                    format!("recipe {name} opens in a new worktree; give it a branch"),
                );
                return;
            };
            let project = self
                .workspaces
                .get(&workspace)
                .and_then(|entry| entry.project)
                .filter(|project| self.projects.get(project).is_some_and(|p| p.git));
            let Some(project) = project else {
                self.refuse(
                    client,
                    "this workspace is not in a Git repository".to_owned(),
                );
                return;
            };
            let start = recipe
                .worktree
                .as_ref()
                .and_then(|worktree| worktree.from.clone());
            self.new_worktree(
                client,
                project,
                branch,
                start,
                Some((Box::new(recipe), switch)),
            );
            return;
        }
        self.open_recipe(client, workspace, &recipe, switch);
    }

    /// Opens a recipe's tabs in a workspace and tells the client.
    pub(crate) fn open_recipe(
        &mut self,
        client: u64,
        workspace: u64,
        recipe: &Recipe,
        switch: bool,
    ) {
        match self.recipe_tabs(workspace, recipe) {
            Ok(tabs) => {
                if let Some(entry) = self.workspaces.get_mut(&workspace) {
                    entry.active_tab = tabs.first().copied().or(entry.active_tab);
                }
                let shown = self.switch_for(client, workspace, switch);
                // A client already on the workspace shows the first new tab.
                if let (Some(shown), Some(first)) = (shown, tabs.first()) {
                    self.select_tab(shown, *first);
                }
                self.broadcast_workspace();
                self.reply(
                    client,
                    FromDaemon::RecipeOpened {
                        workspace,
                        tabs,
                        client: shown,
                    },
                );
            }
            Err(reason) => self.refuse(client, reason),
        }
    }

    /// Makes a recipe's tabs and starts their panes. A directory that does
    /// not exist stops the recipe before anything starts.
    fn recipe_tabs(&mut self, workspace: u64, recipe: &Recipe) -> Result<Vec<u64>, String> {
        let Some(directory) = self
            .workspaces
            .get(&workspace)
            .map(|entry| entry.directory.clone())
        else {
            return Err(format!("no workspace {workspace}"));
        };
        self.reload_config();
        for (name, pane) in &recipe.panes {
            let dir = pane_dir(&directory, pane.dir.as_deref());
            if !dir.is_dir() {
                return Err(format!(
                    "pane {name} starts in {}, which is not a directory",
                    dir.display()
                ));
            }
        }

        let mut tabs = Vec::new();
        let mut failed = Vec::new();
        for spec in &recipe.tabs {
            let Some(tab) = self.add_tab(workspace, spec.name.clone()) else {
                return Err("could not record a new tab".to_owned());
            };
            tabs.push(tab);
            let names: Vec<String> = spec
                .layout
                .iter()
                .flat_map(recipe::node_panes)
                .chain(spec.floating.iter().map(|floating| floating.pane.clone()))
                .collect();
            let mut leaves = BTreeMap::new();
            let mut records = Vec::new();
            for name in names {
                let Some(pane) = recipe.panes.get(&name) else {
                    continue;
                };
                let cwd = pane_dir(&directory, pane.dir.as_deref())
                    .to_string_lossy()
                    .into_owned();
                let record = Pane {
                    agent: crate::agents::PaneAgent::default(),
                    link: None,
                    cols: 80,
                    rows: 24,
                    state: PaneState::Running,
                    screen: None,
                    launch: self.recipe_launch(pane),
                    spawn_worktree: self.worktree_at(&cwd),
                    cwd,
                    title: String::new(),
                    history: self.config.history,
                    workspace,
                    env: pane
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                };
                let id = self
                    .reserve_pane(&record)
                    .map_err(|error| format!("{error:#}"))?;
                leaves.insert(name.clone(), self.new_leaf(id));
                records.push((name, id, record));
            }
            let root = match &spec.layout {
                Some(node) => Some(layout_node(node, &leaves)?),
                None => None,
            };
            let layout = Layout {
                root,
                floating: spec
                    .floating
                    .iter()
                    .filter_map(|floating| {
                        Some(Floating {
                            leaf: *leaves.get(&floating.pane)?,
                            geometry: Fractions {
                                x: floating.x / 100.0,
                                y: floating.y / 100.0,
                                width: floating.width / 100.0,
                                height: floating.height / 100.0,
                            },
                        })
                    })
                    .collect(),
                zoomed: None,
            };
            let first = layout.leaves().first().map(|leaf| leaf.id);
            let Some(entry) = self.tab_mut(tab) else {
                continue;
            };
            entry.layout = layout;
            entry.focus = first;
            let placed = entry.layout.place(area_cells(entry.area));
            for (name, id, record) in records {
                let inner = placed
                    .iter()
                    .find(|placed| placed.leaf.pane == id)
                    .map(|placed| placed.inner)
                    .unwrap_or_default();
                let record = Pane {
                    agent: crate::agents::PaneAgent::default(),
                    cols: inner.width.max(2),
                    rows: inner.height.max(1),
                    ..record
                };
                if let Err(error) = self.start_pane(id, record, None) {
                    failed.push(format!("pane {name}: {error:#}"));
                    if let (Some(entry), Some(leaf)) = (self.tab_mut(tab), leaves.get(&name)) {
                        entry.layout.remove(leaf.id);
                    }
                }
            }
            self.apply_size(tab);
        }
        tabs.retain(|tab| self.tab(*tab).is_some_and(|entry| !entry.layout.is_empty()));
        if !failed.is_empty() {
            for tab in self.workspace_tabs(workspace) {
                self.drop_empty_tab(tab);
            }
            return Err(format!("some panes did not start: {}", failed.join("; ")));
        }
        Ok(tabs)
    }

    /// What a recipe pane runs: an agent pane the default agent, else the
    /// shell; a command line through `/bin/sh -c`.
    fn recipe_launch(&self, pane: &recipe::Pane) -> Launch {
        match &pane.command {
            Some(Command::Line(line)) => {
                Launch::Command(vec!["/bin/sh".to_owned(), "-c".to_owned(), line.clone()])
            }
            Some(Command::Argv(argv)) => Launch::Command(argv.clone()),
            None if pane.agent => self
                .config
                .default_agent
                .as_deref()
                .and_then(|kind| self.agents.get(kind))
                .map_or(Launch::Shell, |kind| Launch::Command(kind.command.clone())),
            None => Launch::Shell,
        }
    }
}

fn layout_node(node: &Node, leaves: &BTreeMap<String, Leaf>) -> Result<layout::Node, String> {
    let leaf = |name: &String| {
        leaves
            .get(name)
            .copied()
            .ok_or_else(|| format!("no pane {name}"))
    };
    Ok(match node {
        Node::Pane(name) => layout::Node::Leaf(leaf(name)?),
        Node::Split {
            axis,
            children,
            ratios,
        } => layout::Node::Split {
            axis: *axis,
            children: children
                .iter()
                .map(|child| layout_node(child, leaves))
                .collect::<Result<_, _>>()?,
            ratios: ratios.clone(),
        },
        Node::Stack { panes, expanded } => layout::Node::Stack {
            children: panes.iter().map(leaf).collect::<Result<_, _>>()?,
            expanded: *expanded,
        },
    })
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}
