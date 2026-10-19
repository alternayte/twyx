//! Recipes: TOML files that open tabs with their layouts and panes, in a
//! workspace or in a new worktree.
//!
//! Panes are defined once under `[panes.<name>]`. Each `[[tab]]` names them
//! in one inline `layout` tree of `row`, `column` and `stack` nodes, and in a
//! `floating` list. A recipe uses only what the layout tree supports, so it
//! opens the same in both clients.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Spanned;

use crate::config::ConfigError;
use crate::layout::Axis;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    pub tabs: Vec<Tab>,
    pub panes: BTreeMap<String, Pane>,
    /// Set when the recipe opens in a new worktree.
    pub worktree: Option<NewWorktree>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    pub layout: Option<Node>,
    pub floating: Vec<Floating>,
}

/// A layout node: a named pane, or a split or stack of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Pane(String),
    Split {
        axis: Axis,
        children: Vec<Node>,
        /// One share per child, summing to 1.
        ratios: Vec<f32>,
    },
    Stack {
        panes: Vec<String>,
        expanded: usize,
    },
}

/// A floating pane, with its place as percentages of the tab area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Floating {
    pub pane: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    /// `None` runs the user's shell.
    pub command: Option<Command>,
    /// Relative to the workspace directory, unless absolute or under `~/`.
    pub dir: Option<String>,
    pub env: BTreeMap<String, String>,
    /// Runs the configured default agent, else the shell.
    pub agent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Run by the user's shell with `-c`.
    Line(String),
    Argv(Vec<String>),
}

impl Command {
    /// The command as the trust prompt shows it.
    pub fn display(&self) -> String {
        match self {
            Command::Line(line) => line.clone(),
            Command::Argv(argv) => argv.join(" "),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewWorktree {
    /// Where the new branch starts; the base branch when `None`.
    pub from: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    tab: Vec<RawTab>,
    #[serde(default)]
    panes: BTreeMap<String, Spanned<RawPane>>,
    worktree: Option<RawWorktree>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTab {
    name: Option<String>,
    layout: Option<Spanned<toml::Value>>,
    #[serde(default)]
    floating: Vec<Spanned<RawFloating>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFloating {
    pane: String,
    #[serde(default = "default_offset")]
    x: f32,
    #[serde(default = "default_offset")]
    y: f32,
    #[serde(default = "default_extent")]
    width: f32,
    #[serde(default = "default_extent")]
    height: f32,
}

fn default_offset() -> f32 {
    20.0
}

fn default_extent() -> f32 {
    60.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPane {
    command: Option<RawCommand>,
    dir: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    agent: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawCommand {
    Line(String),
    Argv(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorktree {
    from: Option<String>,
}

/// Reads a recipe file. Its name is the file stem unless the file names it.
pub fn parse(file: &Path, text: &str) -> Result<Recipe, ConfigError> {
    let fail = |at: Option<std::ops::Range<usize>>, reason: String| ConfigError {
        file: file.to_path_buf(),
        line: at.map_or(1, |at| {
            text[..at.start.min(text.len())].matches('\n').count() + 1
        }),
        reason,
    };
    let raw: RawRecipe = toml::from_str(text).map_err(|error| {
        fail(
            error.span(),
            error.message().trim_end_matches('\n').to_owned(),
        )
    })?;
    let name = raw.name.unwrap_or_else(|| {
        file.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    let mut panes = BTreeMap::new();
    for (pane_name, raw_pane) in raw.panes {
        let span = raw_pane.span();
        let raw_pane = raw_pane.into_inner();
        let command = match raw_pane.command {
            Some(RawCommand::Line(line)) if line.trim().is_empty() => {
                return Err(fail(
                    Some(span),
                    format!("pane {pane_name} has an empty command"),
                ));
            }
            Some(RawCommand::Argv(argv)) if argv.is_empty() => {
                return Err(fail(
                    Some(span),
                    format!("pane {pane_name} has an empty command"),
                ));
            }
            Some(RawCommand::Line(line)) => Some(Command::Line(line)),
            Some(RawCommand::Argv(argv)) => Some(Command::Argv(argv)),
            None => None,
        };
        if raw_pane.agent && command.is_some() {
            return Err(fail(
                Some(span),
                format!("pane {pane_name} is an agent pane and has a command; give one"),
            ));
        }
        if raw_pane.dir.as_deref().is_some_and(str::is_empty) {
            return Err(fail(
                Some(span),
                format!("pane {pane_name} has an empty dir"),
            ));
        }
        panes.insert(
            pane_name,
            Pane {
                command,
                dir: raw_pane.dir,
                env: raw_pane.env,
                agent: raw_pane.agent,
            },
        );
    }

    let mut used = BTreeSet::new();
    let mut claim = |pane: &str, at: std::ops::Range<usize>| {
        if !panes.contains_key(pane) {
            return Err(fail(
                Some(at),
                format!("no pane {pane}; define it in [panes.{pane}]"),
            ));
        }
        if !used.insert(pane.to_owned()) {
            return Err(fail(Some(at), format!("pane {pane} is used twice")));
        }
        Ok(())
    };
    let mut tabs = Vec::new();
    for (index, raw_tab) in raw.tab.into_iter().enumerate() {
        let layout = match raw_tab.layout {
            Some(layout) => {
                let span = layout.span();
                let node =
                    node(layout.get_ref()).map_err(|reason| fail(Some(span.clone()), reason))?;
                for pane in node_panes(&node) {
                    claim(&pane, span.clone())?;
                }
                Some(node)
            }
            None => None,
        };
        let mut floating = Vec::new();
        for raw_floating in raw_tab.floating {
            let span = raw_floating.span();
            let raw_floating = raw_floating.into_inner();
            claim(&raw_floating.pane, span.clone())?;
            let inside = |value: f32| (0.0..=100.0).contains(&value);
            if !(inside(raw_floating.x)
                && inside(raw_floating.y)
                && raw_floating.width > 0.0
                && raw_floating.height > 0.0
                && inside(raw_floating.x + raw_floating.width)
                && inside(raw_floating.y + raw_floating.height))
            {
                return Err(fail(
                    Some(span),
                    format!(
                        "floating pane {} must lie inside the tab: percentages from 0 to 100",
                        raw_floating.pane
                    ),
                ));
            }
            floating.push(Floating {
                pane: raw_floating.pane,
                x: raw_floating.x,
                y: raw_floating.y,
                width: raw_floating.width,
                height: raw_floating.height,
            });
        }
        if layout.is_none() && floating.is_empty() {
            return Err(fail(None, format!("tab {} has no panes", index + 1)));
        }
        tabs.push(Tab {
            name: raw_tab.name.unwrap_or_default(),
            layout,
            floating,
        });
    }
    if tabs.is_empty() {
        return Err(fail(None, "a recipe needs at least one [[tab]]".to_owned()));
    }
    if let Some(unused) = panes.keys().find(|pane| !used.contains(*pane)) {
        return Err(fail(None, format!("pane {unused} is in no tab")));
    }

    Ok(Recipe {
        name,
        description: raw.description.unwrap_or_default(),
        tabs,
        panes,
        worktree: raw.worktree.map(|worktree| NewWorktree {
            from: worktree.from,
        }),
    })
}

/// One layout node from its inline table, or a pane name.
fn node(value: &toml::Value) -> Result<Node, String> {
    let table = match value {
        toml::Value::String(pane) => return Ok(Node::Pane(pane.clone())),
        toml::Value::Table(table) => table,
        other => {
            return Err(format!(
                "a layout node is a pane name or a table, not {other}"
            ));
        }
    };
    let kinds: Vec<&str> = ["row", "column", "stack"]
        .into_iter()
        .filter(|kind| table.contains_key(*kind))
        .collect();
    let [kind] = kinds.as_slice() else {
        return Err("a layout table has exactly one of row, column and stack".to_owned());
    };
    let allowed: &[&str] = match *kind {
        "stack" => &["stack", "expanded"],
        _ => &[kind, "sizes"],
    };
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{key} does not belong in a {kind} node"));
    }
    let Some(toml::Value::Array(items)) = table.get(*kind) else {
        return Err(format!("{kind} takes a list"));
    };
    if items.is_empty() {
        return Err(format!("{kind} is empty"));
    }
    if *kind == "stack" {
        let panes = items
            .iter()
            .map(|item| match item {
                toml::Value::String(pane) => Ok(pane.clone()),
                _ => Err("a stack holds pane names only".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let expanded = match table.get("expanded") {
            None => 0,
            Some(toml::Value::Integer(index)) if *index >= 0 && (*index as usize) < panes.len() => {
                *index as usize
            }
            Some(_) => {
                return Err(format!(
                    "expanded is a position from 0 to {}",
                    panes.len() - 1
                ));
            }
        };
        return Ok(Node::Stack { panes, expanded });
    }
    let children = items.iter().map(node).collect::<Result<Vec<_>, _>>()?;
    let ratios = match table.get("sizes") {
        None => vec![1.0 / children.len() as f32; children.len()],
        Some(toml::Value::Array(sizes)) => {
            let sizes = sizes
                .iter()
                .map(|size| match size {
                    toml::Value::Integer(size) if *size > 0 => Ok(*size as f32),
                    toml::Value::Float(size) if *size > 0.0 => Ok(*size as f32),
                    _ => Err("sizes are positive numbers".to_owned()),
                })
                .collect::<Result<Vec<f32>, _>>()?;
            if sizes.len() != children.len() {
                return Err(format!(
                    "{kind} has {} children and {} sizes",
                    children.len(),
                    sizes.len()
                ));
            }
            let total: f32 = sizes.iter().sum();
            sizes.iter().map(|size| size / total).collect()
        }
        Some(_) => return Err("sizes is a list of numbers".to_owned()),
    };
    Ok(Node::Split {
        axis: if *kind == "row" {
            Axis::Row
        } else {
            Axis::Column
        },
        children,
        ratios,
    })
}

/// Every pane a node names, in order.
pub fn node_panes(node: &Node) -> Vec<String> {
    match node {
        Node::Pane(pane) => vec![pane.clone()],
        Node::Split { children, .. } => children.iter().flat_map(node_panes).collect(),
        Node::Stack { panes, .. } => panes.clone(),
    }
}

/// The recipe twyx always has: Neovim, an agent pane and a test pane.
pub const EDITOR_AGENT_TESTS: &str = r#"
name = "editor-agent-tests"
description = "Neovim, the default agent (else a shell) and a shell for tests"

[[tab]]
name = "code"
layout = { row = ["editor", { column = ["agent", "tests"] }], sizes = [60, 40] }

[panes.editor]
command = ["nvim"]

[panes.agent]
agent = true

[panes.tests]
"#;

/// Every built-in recipe.
pub fn built_in() -> Vec<Recipe> {
    parse(Path::new("editor-agent-tests.toml"), EDITOR_AGENT_TESTS)
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn read(text: &str) -> Result<Recipe, ConfigError> {
        parse(Path::new("dev.toml"), text)
    }

    #[test]
    fn a_recipe_reads_its_tree_panes_and_floating_layer() {
        let recipe = read(
            r#"
[[tab]]
name = "code"
layout = { row = ["editor", { stack = ["a", "b"], expanded = 1 }], sizes = [3, 1] }
floating = [{ pane = "scratch", x = 10, width = 50 }]

[panes.editor]
command = "nvim ."
dir = "src"
env = { A = "1" }

[panes.a]
command = ["cargo", "test"]

[panes.b]
agent = true

[panes.scratch]

[worktree]
from = "main"
"#,
        )
        .expect("a recipe");
        assert_eq!(recipe.name, "dev");
        assert_eq!(
            recipe.tabs[0].layout,
            Some(Node::Split {
                axis: Axis::Row,
                children: vec![
                    Node::Pane("editor".to_owned()),
                    Node::Stack {
                        panes: vec!["a".to_owned(), "b".to_owned()],
                        expanded: 1,
                    },
                ],
                ratios: vec![0.75, 0.25],
            })
        );
        assert_eq!(recipe.tabs[0].floating[0].y, 20.0);
        assert_eq!(
            recipe.panes["editor"].command,
            Some(Command::Line("nvim .".to_owned()))
        );
        assert_eq!(recipe.panes["editor"].env["A"], "1");
        assert!(recipe.panes["b"].agent);
        assert_eq!(recipe.panes["scratch"], Pane::default());
        assert_eq!(recipe.worktree.unwrap().from.as_deref(), Some("main"));
    }

    #[test]
    fn errors_name_the_line_and_the_reason() {
        let cases = [
            ("[[tab]]\nlayout = { row = [\"x\"] }\n", 2, "no pane x"),
            (
                "[[tab]]\nlayout = { row = [\"x\", \"x\"] }\n[panes.x]\n",
                2,
                "used twice",
            ),
            (
                "[[tab]]\nlayout = { row = [\"x\"], column = [\"x\"] }\n[panes.x]\n",
                2,
                "exactly one",
            ),
            (
                "[[tab]]\nlayout = { row = [\"x\", \"y\"], sizes = [1] }\n[panes.x]\n[panes.y]\n",
                2,
                "2 children and 1 sizes",
            ),
            (
                "[[tab]]\nlayout = { stack = [{ row = [\"x\"] }] }\n[panes.x]\n",
                2,
                "pane names only",
            ),
            (
                "[[tab]]\nfloating = [{ pane = \"x\", x = 80, width = 40 }]\n[panes.x]\n",
                2,
                "inside the tab",
            ),
            (
                "[[tab]]\nlayout = \"x\"\n[panes.x]\n[panes.y]\n",
                1,
                "pane y is in no tab",
            ),
            (
                "[[tab]]\nlayout = \"x\"\n\n[panes.x]\nagent = true\ncommand = \"a\"\n",
                4,
                "agent pane",
            ),
            ("name = \"x\"\n", 1, "at least one"),
            (
                "[[tab]]\nlayout = \"x\"\n[panes.x]\nshell = 1\n",
                4,
                "unknown field",
            ),
        ];
        for (text, line, reason) in cases {
            let error = read(text).expect_err(text);
            assert!(error.reason.contains(reason), "{text}: {}", error.reason);
            assert_eq!(error.line, line, "{text}: {}", error.reason);
        }
    }

    #[test]
    fn the_built_in_recipe_reads() {
        let recipes = built_in();
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].name, "editor-agent-tests");
        assert_eq!(recipes[0].panes.len(), 3);
    }
}
