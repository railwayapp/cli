//! First-run setup, inside the TUI.
//!
//! The same questions `railway ca setup` asks, in the same order, but as a
//! sequence of cards in the TUI's own style rather than a stack of inquire
//! prompts. It runs when there are no preferences yet, which is exactly the
//! moment someone has no idea what a "target" or a "harness" is, so each step
//! carries a line saying what the choice does.
//!
//! The target step reads from the tree the TUI already loaded, so browsing it
//! costs no network: workspaces list first, and expanding one (or a project
//! inside it) reveals what is under it, exactly like the tree on the Manage
//! screen. Creating a project is the only step that can fail.

use super::app::WorkspaceNode;
use super::theme::{THEMES, Theme};

/// One question in the flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The modal that opens it: set up now, or not.
    Intro,
    /// Workspace → project → environment, expandable in place.
    Target,
    Agent,
    Skills,
    Theme,
}

/// A project the wizard can adopt as the default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectOption {
    pub project_id: String,
    pub project_name: String,
    pub environment_id: String,
    pub environment_name: String,
}

/// What the wizard asks for when it finishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub project: Option<ProjectOption>,
    pub agent: String,
    pub skills: bool,
    pub skills_source: Option<String>,
    pub theme: String,
    /// Carried through untouched — the wizard never asks; only the ⌥s
    /// settings card changes it.
    pub hide_tabs: bool,
}

/// An environment leaf under a [`TargetProject`]. Selecting one finishes the
/// target step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetEnv {
    pub id: String,
    pub name: String,
}

/// A project under a [`TargetWorkspace`]. Expands to show its environments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetProject {
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub envs: Vec<TargetEnv>,
}

/// A workspace, the top level of the target step. Expands to show its
/// projects, and (once expanded) a row to create a new project inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetWorkspace {
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub projects: Vec<TargetProject>,
}

/// One visible row of the target step's flattened, indented tree. Recomputed
/// from `Wizard::workspaces` on every render and every keypress, so it never
/// drifts from the expand state that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TargetRow {
    Workspace(usize),
    /// Create a project inside workspace `usize` — the first row under it
    /// once expanded, ahead of its existing projects.
    CreateProject(usize),
    Project(usize, usize),
    Env(usize, usize, usize),
    /// Pinned last: a way out at any depth.
    DecideLater,
}

pub struct Wizard {
    pub step: Step,
    pub cursor: usize,
    /// Set while a step is doing something slow, e.g. creating a project.
    pub busy: Option<String>,
    /// Shown under the card when a step went wrong.
    pub error: Option<String>,
    pub project: Option<ProjectOption>,
    pub workspaces: Vec<TargetWorkspace>,
    pub agent: usize,
    pub skills: bool,
    pub skills_source: Option<String>,
    pub theme: usize,
    /// Not a step — carried so finishing the flow doesn't reset it.
    pub hide_tabs: bool,
}

/// What a keypress asked the loop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Redraw only; the theme step previews as the cursor moves.
    Redraw,
    /// Create the default project in this workspace, then call
    /// [`Wizard::project_created`].
    CreateProject(String),
    /// The user finished; save these.
    Finish(Box<Outcome>),
    /// The user declined or backed out of the first question.
    Cancel,
}

/// The projects a card can offer as the default, from the tree the TUI
/// already loaded. Shared with the settings card, which asks the same
/// question after the fact.
pub fn project_options(tree: &[WorkspaceNode]) -> Vec<ProjectOption> {
    tree.iter()
        .flat_map(|ws| ws.projects.iter())
        .filter_map(|project| {
            let env = project.envs.first()?;
            Some(ProjectOption {
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                environment_id: env.id.clone(),
                environment_name: env.name.clone(),
            })
        })
        .collect()
}

/// What each harness is, for the cards that offer them.
pub fn harness_blurb(slug: &str) -> &'static str {
    match slug {
        "claude" => "Anthropic's Claude Code",
        "codex" => "OpenAI's Codex",
        "grok" => "xAI's Grok",
        "railway" => "Railway's own agent — no sign-in needed",
        "shell" => "No agent — just a shell on the VM",
        // Named rather than folded into a catch-all: an unknown slug is a
        // preferences file someone hand-edited, and labelling it as whichever
        // harness happens to sit in the `_` arm is worse than saying nothing.
        _ => "",
    }
}

impl Wizard {
    /// Build the flow, taking the workspace tree from the loaded tree. The
    /// first workspace opens by default — mirroring the Manage screen, so the
    /// target step is never a wall of collapsed rows — and everything under
    /// it stays collapsed until asked for.
    pub fn new(
        tree: &[WorkspaceNode],
        harness: Option<&str>,
        theme: &Theme,
        skills_source: Option<String>,
        hide_tabs: bool,
    ) -> Self {
        let mut workspaces: Vec<TargetWorkspace> = tree
            .iter()
            .map(|ws| TargetWorkspace {
                id: ws.id.clone(),
                name: ws.name.clone(),
                expanded: false,
                projects: ws
                    .projects
                    .iter()
                    .map(|project| TargetProject {
                        id: project.id.clone(),
                        name: project.name.clone(),
                        expanded: false,
                        envs: project
                            .envs
                            .iter()
                            .map(|env| TargetEnv {
                                id: env.id.clone(),
                                name: env.name.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect();
        if let Some(first) = workspaces.first_mut() {
            first.expanded = true;
        }
        Self {
            step: Step::Intro,
            cursor: 0,
            busy: None,
            error: None,
            project: None,
            workspaces,
            agent: harness
                .and_then(|h| super::app::default_harnesses().iter().position(|x| *x == h))
                .unwrap_or(0),
            skills: skills_source.is_some(),
            skills_source,
            theme: theme.index(),
            hide_tabs,
        }
    }

    /// The target step's rows, flattened and in display order: each
    /// workspace, then (if expanded) a create-project row and its projects,
    /// then (for each expanded project) its environments — "Decide later"
    /// pinned at the end regardless of depth.
    fn target_rows(&self) -> Vec<TargetRow> {
        let mut rows = Vec::new();
        for (w, workspace) in self.workspaces.iter().enumerate() {
            rows.push(TargetRow::Workspace(w));
            if !workspace.expanded {
                continue;
            }
            rows.push(TargetRow::CreateProject(w));
            for (p, project) in workspace.projects.iter().enumerate() {
                rows.push(TargetRow::Project(w, p));
                // A single environment shows inline on the project row and
                // selecting the project picks it — a one-row expansion would
                // be a second keypress that adds no information.
                if !project.expanded || project.envs.len() == 1 {
                    continue;
                }
                for e in 0..project.envs.len() {
                    rows.push(TargetRow::Env(w, p, e));
                }
            }
        }
        rows.push(TargetRow::DecideLater);
        rows
    }

    /// The label and description for one target row, indented by depth. Only
    /// workspaces and projects carry an expand marker — an environment is
    /// always a leaf.
    fn target_row_label(&self, row: TargetRow) -> (String, String) {
        match row {
            TargetRow::Workspace(w) => {
                let workspace = &self.workspaces[w];
                let marker = if workspace.expanded { "▾" } else { "▸" };
                let count = workspace.projects.len();
                (
                    format!("{marker} {}", workspace.name),
                    format!("{count} project{}", if count == 1 { "" } else { "s" }),
                )
            }
            TargetRow::CreateProject(w) => (
                "    + Create a project".into(),
                format!(
                    "a new project named \"Cloud Agents\" in {}",
                    self.workspaces[w].name
                ),
            ),
            TargetRow::Project(w, p) => {
                let project = &self.workspaces[w].projects[p];
                let count = project.envs.len();
                // One environment: name it in place and let enter pick it.
                // The expand marker would open a one-row list saying the same
                // thing the parentheses already say.
                if let [env] = project.envs.as_slice() {
                    return (
                        format!("    {} ({})", project.name, env.name),
                        String::new(),
                    );
                }
                let marker = if project.expanded { "▾" } else { "▸" };
                (
                    format!("  {marker} {}", project.name),
                    format!("{count} environment{}", if count == 1 { "" } else { "s" }),
                )
            }
            TargetRow::Env(w, p, e) => {
                let env = &self.workspaces[w].projects[p].envs[e];
                (format!("    {}", env.name), String::new())
            }
            TargetRow::DecideLater => (
                "Decide later".into(),
                "Pick a target each time you launch".into(),
            ),
        }
    }

    /// The options on the current card: (label, what it does).
    pub fn options(&self) -> Vec<(String, String)> {
        match self.step {
            Step::Intro => vec![
                (
                    "Set up now".into(),
                    "Four questions: where agents live, which one to run, your skills, and a theme"
                        .into(),
                ),
                (
                    "Not now".into(),
                    "Everything still works; you will be asked for a target each time".into(),
                ),
            ],
            Step::Target => self
                .target_rows()
                .into_iter()
                .map(|row| self.target_row_label(row))
                .collect(),
            // The default-able harnesses only: `shell` is a way to launch a
            // session, not a default agent to save.
            Step::Agent => super::app::default_harnesses()
                .iter()
                .map(|slug| ((*slug).to_string(), harness_blurb(slug).to_string()))
                .collect(),
            Step::Skills => vec![
                (
                    "Bring my skills".into(),
                    match self.skills_source.as_deref() {
                        Some(source) => format!("copy the skills in your {source} directory"),
                        None => "no skills found on this machine".into(),
                    },
                ),
                (
                    "Do not sync skills".into(),
                    "agents run with Railway's own skills only".into(),
                ),
            ],
            Step::Theme => THEMES
                .iter()
                .map(|theme| (theme.label.to_string(), String::new()))
                .collect(),
        }
    }

    /// Start at the first question rather than at "do you want to?".
    pub fn skip_intro(&mut self) {
        self.step = Step::Target;
        self.cursor = 0;
    }

    pub fn title(&self) -> &'static str {
        match self.step {
            Step::Intro => "Set up cloud agents?",
            Step::Target => "Where should agents live?",
            Step::Agent => "Which coding agent?",
            Step::Skills => "Bring your skills?",
            Step::Theme => "Pick a theme",
        }
    }

    /// Which step this is, for the progress dots. `None` on the intro, which is
    /// a question about whether to start rather than part of the flow.
    pub fn position(&self) -> Option<(usize, usize)> {
        let index = match self.step {
            Step::Intro => return None,
            Step::Target => 0,
            Step::Agent => 1,
            Step::Skills => 2,
            Step::Theme => 3,
        };
        Some((index, 4))
    }

    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.preview_theme();
    }

    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.options().len().saturating_sub(1));
        self.preview_theme();
    }

    /// The theme step applies as the cursor moves — a colour scheme is not
    /// something anyone can pick from a name.
    fn preview_theme(&mut self) {
        if self.step == Step::Theme {
            self.theme = self.cursor.min(THEMES.len() - 1);
        }
    }

    pub fn previewed_theme(&self) -> &'static Theme {
        &THEMES[self.theme.min(THEMES.len() - 1)]
    }

    /// Enter on the highlighted option.
    pub fn select(&mut self) -> Action {
        self.error = None;
        match self.step {
            Step::Intro => {
                if self.cursor == 1 {
                    return Action::Cancel;
                }
                self.go(Step::Target);
                Action::Redraw
            }
            Step::Target => match self.target_rows().get(self.cursor).copied() {
                Some(TargetRow::Workspace(w)) => {
                    let workspace = &mut self.workspaces[w];
                    workspace.expanded = !workspace.expanded;
                    Action::Redraw
                }
                Some(TargetRow::Project(w, p)) => {
                    let project = &mut self.workspaces[w].projects[p];
                    // A single environment is what the row already says;
                    // enter picks it rather than expanding a one-row list.
                    if let [env] = project.envs.as_slice() {
                        self.project = Some(ProjectOption {
                            project_id: project.id.clone(),
                            project_name: project.name.clone(),
                            environment_id: env.id.clone(),
                            environment_name: env.name.clone(),
                        });
                        self.go(Step::Agent);
                        return Action::Redraw;
                    }
                    project.expanded = !project.expanded;
                    Action::Redraw
                }
                Some(TargetRow::Env(w, p, e)) => {
                    let project = &self.workspaces[w].projects[p];
                    let env = &project.envs[e];
                    self.project = Some(ProjectOption {
                        project_id: project.id.clone(),
                        project_name: project.name.clone(),
                        environment_id: env.id.clone(),
                        environment_name: env.name.clone(),
                    });
                    self.go(Step::Agent);
                    Action::Redraw
                }
                Some(TargetRow::CreateProject(w)) => {
                    let workspace = &self.workspaces[w];
                    self.busy = Some(format!("Creating Cloud Agents in {}…", workspace.name));
                    Action::CreateProject(workspace.id.clone())
                }
                Some(TargetRow::DecideLater) | None => {
                    self.project = None;
                    self.go(Step::Agent);
                    Action::Redraw
                }
            },
            Step::Agent => {
                self.agent = self.cursor.min(super::app::default_harnesses().len() - 1);
                self.go(Step::Skills);
                Action::Redraw
            }
            Step::Skills => {
                // "Bring my skills" with nothing to bring is not an option.
                self.skills = self.cursor == 0 && self.skills_source.is_some();
                self.go(Step::Theme);
                Action::Redraw
            }
            Step::Theme => {
                self.theme = self.cursor.min(THEMES.len() - 1);
                Action::Finish(Box::new(Outcome {
                    project: self.project.clone(),
                    agent: super::app::default_harnesses()[self.agent].to_string(),
                    skills: self.skills,
                    skills_source: self.skills_source.clone(),
                    theme: THEMES[self.theme].slug.to_string(),
                    hide_tabs: self.hide_tabs,
                }))
            }
        }
    }

    /// Escape: back a step, or out of the flow from the first one.
    pub fn back(&mut self) -> Action {
        match self.step {
            Step::Intro => Action::Cancel,
            Step::Target => {
                self.go(Step::Intro);
                Action::Redraw
            }
            Step::Agent => {
                self.go(Step::Target);
                Action::Redraw
            }
            Step::Skills => {
                self.go(Step::Agent);
                Action::Redraw
            }
            Step::Theme => {
                self.go(Step::Skills);
                Action::Redraw
            }
        }
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.cursor = match step {
            Step::Agent => self.agent,
            Step::Theme => self.theme,
            _ => 0,
        };
    }

    /// The create-project step finished, one way or the other.
    pub fn project_created(&mut self, result: Result<ProjectOption, String>) {
        self.busy = None;
        match result {
            Ok(project) => {
                self.project = Some(project);
                self.go(Step::Agent);
            }
            Err(err) => self.error = Some(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::cloud_agent::tui::app::{EnvNode, Load, ProjectNode};

    fn tree() -> Vec<WorkspaceNode> {
        vec![WorkspaceNode {
            id: "ws1".into(),
            name: "Railway".into(),
            expanded: true,
            projects: vec![
                ProjectNode {
                    id: "p1".into(),
                    name: "devtools".into(),
                    expanded: false,
                    envs: vec![EnvNode {
                        id: "e1".into(),
                        name: "production".into(),
                        expanded: false,
                        agents: Load::NotLoaded,
                    }],
                },
                ProjectNode {
                    id: "p2".into(),
                    name: "mono".into(),
                    expanded: false,
                    envs: vec![EnvNode {
                        id: "e2".into(),
                        name: "staging".into(),
                        expanded: false,
                        agents: Load::NotLoaded,
                    }],
                },
            ],
        }]
    }

    fn wizard() -> Wizard {
        Wizard::new(&tree(), Some("codex"), Theme::default_theme(), None, false)
    }

    /// More than one environment still expands: the parentheses shortcut is
    /// only for the project whose environment was never a choice.
    #[test]
    fn a_multi_env_project_still_expands() {
        let mut base = tree();
        base[0].projects[0].envs.push(EnvNode {
            id: "e9".into(),
            name: "staging".into(),
            expanded: false,
            agents: Load::NotLoaded,
        });
        let mut w = Wizard::new(&base, None, Theme::default_theme(), None, false);
        w.skip_intro();
        let devtools = w
            .options()
            .iter()
            .position(|(label, _)| label.contains("devtools"))
            .unwrap();
        assert!(
            w.options()[devtools].0.contains('▸'),
            "two environments keep the expand marker"
        );
        w.cursor = devtools;
        assert_eq!(w.select(), Action::Redraw);
        assert_eq!(w.step, Step::Target, "expanding is not picking");
        assert!(
            w.options()
                .iter()
                .any(|(label, _)| label.trim_start().starts_with("production")),
            "{:?}",
            w.options()
        );
    }

    #[test]
    fn declining_the_intro_leaves_without_saving() {
        let mut w = wizard();
        w.down();
        assert_eq!(w.select(), Action::Cancel);
    }

    /// The first workspace opens by default, so its projects are visible
    /// right away and nothing about them is listed until expanded.
    #[test]
    fn the_first_workspace_opens_by_default() {
        let mut w = wizard();
        w.skip_intro();
        let options = w.options();
        assert_eq!(options[0].0, "▾ Railway");
        // A single environment rides inline on its project's row: there is
        // nothing an expansion would reveal that the parentheses don't say.
        assert!(
            options
                .iter()
                .any(|(label, _)| label.contains("devtools (production)")),
            "{options:?}"
        );
        assert!(
            options
                .iter()
                .any(|(label, _)| label.contains("mono (staging)")),
            "{options:?}"
        );
    }

    /// The whole flow, ending in the preferences it collected: expand into a
    /// project, then pick one of its environments.
    #[test]
    fn the_flow_collects_every_answer() {
        let mut w = wizard();
        assert_eq!(w.select(), Action::Redraw); // set up now
        assert_eq!(w.step, Step::Target);

        // Rows: ▾ Railway, + Create a project, devtools (production),
        // mono (staging), Decide later.
        w.down(); // create a project
        w.down(); // devtools
        w.down(); // mono
        // A single-environment project needs no expansion: enter picks it,
        // environment and all.
        w.select();
        assert_eq!(w.step, Step::Agent);
        assert_eq!(w.project.as_ref().unwrap().project_name, "mono");
        // The environment picked comes along with it.
        assert_eq!(w.project.as_ref().unwrap().environment_name, "staging");

        // The agent step opens on the harness already configured.
        assert_eq!(w.cursor, 2, "codex was passed in");
        w.select();
        assert_eq!(w.step, Step::Skills);

        w.select(); // "bring my skills" with nothing to bring
        assert_eq!(w.step, Step::Theme);

        w.down();
        let Action::Finish(outcome) = w.select() else {
            panic!("expected the flow to finish");
        };
        assert_eq!(outcome.agent, "codex");
        assert!(!outcome.skills, "nothing to bring, so nothing is promised");
        assert_eq!(outcome.project.unwrap().project_id, "p2");
        assert_eq!(outcome.theme, THEMES[1].slug);
    }

    /// Creating a project is the one step that can fail; it says so and stays
    /// put rather than continuing as though it worked.
    #[test]
    fn a_failed_create_keeps_the_step() {
        let mut w = wizard();
        w.select(); // set up now
        w.down(); // + Create a project, under the already-open workspace
        assert_eq!(w.select(), Action::CreateProject("ws1".into()));
        assert!(w.busy.is_some());

        w.project_created(Err("no permission".into()));
        assert_eq!(w.step, Step::Target);
        assert!(w.busy.is_none());
        assert_eq!(w.error.as_deref(), Some("no permission"));

        w.project_created(Ok(ProjectOption {
            project_id: "new".into(),
            project_name: "Cloud Agents".into(),
            environment_id: "env".into(),
            environment_name: "production".into(),
        }));
        assert_eq!(w.step, Step::Agent);
        assert_eq!(w.project.as_ref().unwrap().project_name, "Cloud Agents");
    }

    /// Escape walks back a step at a time, and out from the first.
    #[test]
    fn escape_walks_back() {
        let mut w = wizard();
        w.select();
        w.down();
        w.select(); // create → busy, but the step is what matters here
        w.step = Step::Agent;
        assert_eq!(w.back(), Action::Redraw);
        assert_eq!(w.step, Step::Target);
        assert_eq!(w.back(), Action::Redraw);
        assert_eq!(w.step, Step::Intro);
        assert_eq!(w.back(), Action::Cancel);
    }

    /// A theme is picked by looking at it, so the cursor previews.
    #[test]
    fn the_theme_step_previews_as_you_move() {
        let mut w = wizard();
        w.step = Step::Theme;
        w.cursor = 0;
        let first = w.previewed_theme().slug;
        w.down();
        assert_ne!(w.previewed_theme().slug, first);
    }

    /// Chosen from the menu, setup starts at the first question: "do you want
    /// to set up?" has already been answered by choosing it.
    #[test]
    fn skipping_the_intro_starts_at_the_first_question() {
        let mut w = wizard();
        w.skip_intro();
        assert_eq!(w.step, Step::Target);
        assert_eq!(w.position(), Some((0, 4)));

        // And escape from there goes back to the intro rather than out, so the
        // flow is still walkable in both directions.
        assert_eq!(w.back(), Action::Redraw);
        assert_eq!(w.step, Step::Intro);
    }

    /// With no workspaces at all there is nothing to expand into, so the only
    /// way through is the escape hatch.
    #[test]
    fn with_no_workspaces_only_decide_later_is_offered() {
        let mut w = Wizard::new(&[], None, Theme::default_theme(), None, false);
        w.select(); // set up now
        let labels: Vec<String> = w.options().into_iter().map(|(label, _)| label).collect();
        assert_eq!(labels, vec!["Decide later"]);

        w.select();
        assert_eq!(w.step, Step::Agent);
        assert!(w.project.is_none());
    }

    /// A second workspace stays collapsed until asked for, and expanding it
    /// does not disturb the first.
    #[test]
    fn other_workspaces_stay_collapsed_until_expanded() {
        let mut tree = tree();
        tree.push(WorkspaceNode {
            id: "ws2".into(),
            name: "Personal".into(),
            expanded: false,
            projects: vec![ProjectNode {
                id: "p3".into(),
                name: "side-project".into(),
                expanded: false,
                envs: vec![EnvNode {
                    id: "e3".into(),
                    name: "production".into(),
                    expanded: false,
                    agents: Load::NotLoaded,
                }],
            }],
        });
        let mut w = Wizard::new(&tree, Some("codex"), Theme::default_theme(), None, false);
        w.skip_intro();

        let before = w.options();
        assert!(
            !before
                .iter()
                .any(|(label, _)| label.contains("side-project"))
        );

        let personal_row = before
            .iter()
            .position(|(label, _)| label.contains("Personal"))
            .expect("the second workspace row");
        w.cursor = personal_row;
        w.select();

        let after = w.options();
        assert!(
            after
                .iter()
                .any(|(label, _)| label.contains("side-project"))
        );
        assert!(
            after.iter().any(|(label, _)| label.contains("devtools")),
            "the first workspace is still expanded"
        );
    }
}
