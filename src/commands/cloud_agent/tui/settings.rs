//! The ⌥s settings card.
//!
//! Every preference first-run setup collects, on one card, changeable after
//! the fact. The wizard asks its questions once, in order, for someone who has
//! never answered them; this is where the answers live afterwards — each one
//! visible with its current value, and changeable without walking a flow.
//!
//! Values with a handful of options (agent, skills, theme) cycle in place with
//! ←/→ and save on every change, so escape is only ever "close" — there is no
//! dirty state to confirm away. The default project is the one exception: a
//! project list is too long to flick through blind, so its row opens the same
//! card the wizard's project step uses, and comes straight back here.

use super::app::{WorkspaceNode, default_harnesses};
use super::theme::{THEMES, Theme};
use super::wizard::{Outcome, ProjectOption, harness_blurb, project_options};

/// One row of the card, top to bottom — the wizard's questions, in its order,
/// plus a way back into the flow itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Agent,
    Project,
    Skills,
    Theme,
    /// Whether the maximized layout draws its header tabs. Settings-only —
    /// the wizard never asks.
    Tabs,
    /// Replay first-run setup, for anyone who wants the guided walk.
    Setup,
}

const ROWS: &[Row] = &[
    Row::Agent,
    Row::Project,
    Row::Skills,
    Row::Theme,
    Row::Tabs,
    Row::Setup,
];

/// What a keypress asked the loop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    /// A value changed; persist this snapshot of every preference.
    Save(Box<Outcome>),
    /// Create the default project in this workspace, then call
    /// [`Settings::project_created`].
    CreateProject(String),
    /// The last row: replay the first-run flow.
    RunSetup,
    /// Escape from the top level; the card is done.
    Close,
}

pub struct Settings {
    pub cursor: usize,
    /// The project sub-picker's cursor, while it is open.
    pub pick: Option<usize>,
    /// Set while the picker is creating a project.
    pub busy: Option<String>,
    /// Shown under the card when the create went wrong.
    pub error: Option<String>,
    pub projects: Vec<ProjectOption>,
    /// The workspaces a new project could be created in, as (id, name). One
    /// create row each, so the card never has to guess which one a new
    /// project lands in — the rule the wizard's target step follows too.
    pub workspaces: Vec<(String, String)>,
    pub project: Option<ProjectOption>,
    pub agent: usize,
    pub skills: bool,
    pub skills_source: Option<String>,
    pub theme: usize,
    pub hide_tabs: bool,
}

impl Settings {
    pub fn new(
        tree: &[WorkspaceNode],
        project: Option<ProjectOption>,
        agent: usize,
        skills: bool,
        skills_source: Option<String>,
        theme: &Theme,
        hide_tabs: bool,
    ) -> Self {
        Self {
            cursor: 0,
            pick: None,
            busy: None,
            error: None,
            projects: project_options(tree),
            workspaces: tree
                .iter()
                .map(|ws| (ws.id.clone(), ws.name.clone()))
                .collect(),
            project,
            agent: agent.min(default_harnesses().len() - 1),
            // "On" with nothing to sync is not a state; it reads as a promise.
            skills: skills && skills_source.is_some(),
            skills_source,
            theme: theme.index(),
            hide_tabs,
        }
    }

    /// The highlighted row of the main card.
    pub fn row(&self) -> Row {
        ROWS[self.cursor.min(ROWS.len() - 1)]
    }

    /// Whether ←/→ changes the highlighted row in place — what tells the UI to
    /// draw the value in cycle arrows.
    pub fn cycles(&self) -> bool {
        match self.row() {
            Row::Agent | Row::Theme | Row::Tabs => true,
            Row::Skills => self.skills_source.is_some(),
            Row::Project | Row::Setup => false,
        }
    }

    /// The main card's rows: (label, current value, what it does).
    pub fn options(&self) -> Vec<(String, String, String)> {
        ROWS.iter()
            .map(|row| match row {
                Row::Agent => (
                    "Coding agent".into(),
                    default_harnesses()[self.agent].to_string(),
                    harness_blurb(default_harnesses()[self.agent]).into(),
                ),
                Row::Project => (
                    "Default project".into(),
                    self.project.as_ref().map_or("not set".into(), |p| {
                        format!("{} ({})", p.project_name, p.environment_name)
                    }),
                    "Where new cloud agents are created".into(),
                ),
                Row::Skills => (
                    "Skills sync".into(),
                    match (&self.skills_source, self.skills) {
                        (Some(source), true) => format!("on · {source}"),
                        _ => "off".into(),
                    },
                    match &self.skills_source {
                        Some(_) if self.skills => "Copied to the agent at launch".into(),
                        Some(_) => "Agents run with Railway's own skills only".into(),
                        None => "No skills found on this machine".into(),
                    },
                ),
                Row::Theme => (
                    "Theme".into(),
                    THEMES[self.theme].label.to_string(),
                    "Previews as you cycle".into(),
                ),
                Row::Tabs => (
                    "Full-screen tabs".into(),
                    if self.hide_tabs { "hidden" } else { "shown" }.into(),
                    if self.hide_tabs {
                        "⌥⇧[ ⌥⇧] move between sessions".into()
                    } else {
                        "Open sessions as header tabs when maximized".into()
                    },
                ),
                Row::Setup => (
                    "Run first-time setup again".into(),
                    String::new(),
                    String::new(),
                ),
            })
            .collect()
    }

    /// The project sub-picker's rows: (label, tag, detail).
    pub fn picker_options(&self) -> Vec<(String, String, String)> {
        let mut rows: Vec<(String, String, String)> = self
            .projects
            .iter()
            .map(|p| {
                let current = self.project.as_ref().is_some_and(|c| {
                    c.project_id == p.project_id && c.environment_id == p.environment_id
                });
                (
                    format!("{} ({})", p.project_name, p.environment_name),
                    if current {
                        "current default".into()
                    } else {
                        String::new()
                    },
                    String::new(),
                )
            })
            .collect();
        // One row per workspace. With a single workspace there is nothing to
        // disambiguate and it reads as it always did; with several, the row
        // names where the project goes rather than picking one silently.
        let single = self.workspaces.len() == 1;
        for (_, name) in &self.workspaces {
            rows.push((
                if single {
                    "Create a project".to_string()
                } else {
                    format!("Create a project in {name}")
                },
                String::new(),
                "A new Railway project named \"Cloud Agents\" to keep them in".into(),
            ));
        }
        rows.push((
            "Decide later".into(),
            String::new(),
            "Pick a target each time you launch".into(),
        ));
        rows
    }

    /// The theme the whole screen should draw in right now. Applied live, like
    /// the wizard's theme step — a colour scheme is picked by looking at it.
    pub fn current_theme(&self) -> &'static Theme {
        &THEMES[self.theme.min(THEMES.len() - 1)]
    }

    pub fn up(&mut self) {
        self.error = None;
        match self.pick {
            Some(p) => self.pick = Some(p.saturating_sub(1)),
            None => self.cursor = self.cursor.saturating_sub(1),
        }
    }

    pub fn down(&mut self) {
        self.error = None;
        match self.pick {
            Some(p) => self.pick = Some((p + 1).min(self.picker_options().len() - 1)),
            None => self.cursor = (self.cursor + 1).min(ROWS.len() - 1),
        }
    }

    pub fn left(&mut self) -> Action {
        self.cycle(false)
    }

    pub fn right(&mut self) -> Action {
        self.cycle(true)
    }

    /// Enter on the highlighted row. For a value that cycles, enter is another
    /// way to step it forward — a key that did nothing on a row that says
    /// "change me" would read as broken.
    pub fn select(&mut self) -> Action {
        self.error = None;
        if let Some(p) = self.pick {
            return self.pick_select(p);
        }
        match self.row() {
            Row::Project => self.open_picker(),
            Row::Setup => Action::RunSetup,
            _ => self.cycle(true),
        }
    }

    /// Escape: out of the picker, or out of the card.
    pub fn back(&mut self) -> Action {
        self.error = None;
        if self.pick.is_some() {
            self.pick = None;
            return Action::Redraw;
        }
        Action::Close
    }

    /// The project step finished, one way or the other. `Some` carries the
    /// snapshot to persist when the new project stuck.
    pub fn project_created(
        &mut self,
        result: Result<ProjectOption, String>,
    ) -> Option<Box<Outcome>> {
        self.busy = None;
        match result {
            Ok(project) => {
                self.project = Some(project);
                self.pick = None;
                match self.save() {
                    Action::Save(outcome) => Some(outcome),
                    _ => None,
                }
            }
            Err(err) => {
                self.error = Some(err);
                None
            }
        }
    }

    fn cycle(&mut self, forward: bool) -> Action {
        self.error = None;
        if self.pick.is_some() {
            return Action::None;
        }
        match self.row() {
            Row::Agent => {
                // Cycles the default-able harnesses only: `shell` is a way
                // to launch a session, not a default agent to save.
                self.agent = wrap(self.agent, default_harnesses().len(), forward);
                self.save()
            }
            Row::Skills if self.skills_source.is_some() => {
                self.skills = !self.skills;
                self.save()
            }
            Row::Theme => {
                self.theme = wrap(self.theme, THEMES.len(), forward);
                self.save()
            }
            Row::Tabs => {
                self.hide_tabs = !self.hide_tabs;
                self.save()
            }
            // → reads as "into"; ← on a row that opens a card does nothing.
            Row::Project if forward => self.open_picker(),
            _ => Action::None,
        }
    }

    fn open_picker(&mut self) -> Action {
        // Open on the current default, so enter-enter changes nothing.
        let current = self.project.as_ref().and_then(|c| {
            self.projects
                .iter()
                .position(|p| p.project_id == c.project_id && p.environment_id == c.environment_id)
        });
        self.pick = Some(current.unwrap_or(0));
        Action::Redraw
    }

    fn pick_select(&mut self, p: usize) -> Action {
        if let Some(project) = self.projects.get(p) {
            self.project = Some(project.clone());
            self.pick = None;
            return self.save();
        }
        if let Some((id, name)) = self.workspaces.get(p - self.projects.len()) {
            self.busy = Some(match self.workspaces.len() {
                1 => "Creating Cloud Agents…".to_string(),
                _ => format!("Creating Cloud Agents in {name}…"),
            });
            return Action::CreateProject(id.clone());
        }
        // Decide later: clear the default, so every launch asks.
        self.project = None;
        self.pick = None;
        self.save()
    }

    /// The whole card as one snapshot — every save writes every preference, so
    /// there is exactly one shape of write to reason about.
    fn save(&self) -> Action {
        Action::Save(Box::new(Outcome {
            project: self.project.clone(),
            agent: default_harnesses()[self.agent].to_string(),
            skills: self.skills,
            skills_source: self.skills_source.clone(),
            theme: THEMES[self.theme].slug.to_string(),
            hide_tabs: self.hide_tabs,
        }))
    }
}

fn wrap(i: usize, len: usize, forward: bool) -> usize {
    if forward {
        (i + 1) % len
    } else {
        (i + len - 1) % len
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

    fn settings() -> Settings {
        Settings::new(
            &tree(),
            Some(ProjectOption {
                project_id: "p1".into(),
                project_name: "devtools".into(),
                environment_id: "e1".into(),
                environment_name: "production".into(),
            }),
            0,
            true,
            Some("claude".into()),
            Theme::default_theme(),
            false,
        )
    }

    fn saved(action: Action) -> Outcome {
        match action {
            Action::Save(outcome) => *outcome,
            other => panic!("expected a save, got {other:?}"),
        }
    }

    /// Every change is a full snapshot, so one ← on the agent row already
    /// carries every other preference unchanged.
    #[test]
    fn cycling_the_agent_saves_a_full_snapshot() {
        let mut s = settings();
        // The list leads with `railway`, so one step forward from the start
        // lands on claude.
        let outcome = saved(s.right());
        assert_eq!(outcome.agent, "claude");
        assert_eq!(outcome.theme, "railway");
        assert!(outcome.skills);
        assert_eq!(outcome.project.unwrap().project_id, "p1");

        // And it wraps in both directions.
        assert_eq!(saved(s.left()).agent, "railway");
        assert_eq!(saved(s.left()).agent, "grok");
    }

    /// Enter on a cycling row steps it forward — a row that says "change me"
    /// must not have a dead enter key.
    #[test]
    fn enter_cycles_too() {
        let mut s = settings();
        assert_eq!(saved(s.select()).agent, "claude");
    }

    /// The theme row cycles and the card previews it immediately.
    #[test]
    fn the_theme_cycles_and_previews() {
        let mut s = settings();
        s.cursor = 3;
        let first = s.current_theme().slug;
        let outcome = saved(s.right());
        assert_ne!(s.current_theme().slug, first);
        assert_eq!(outcome.theme, s.current_theme().slug);
    }

    /// Skills toggles — but only when there is something to sync. With no
    /// source on the machine the row is inert, and says so in its detail line.
    #[test]
    fn skills_toggle_needs_a_source() {
        let mut s = settings();
        s.cursor = 2;
        assert!(!saved(s.right()).skills, "on toggles off");
        assert!(saved(s.right()).skills, "and back on");

        let mut bare = Settings::new(&tree(), None, 0, true, None, Theme::default_theme(), false);
        assert!(!bare.skills, "enabled with no source is not a state");
        bare.cursor = 2;
        assert_eq!(bare.right(), Action::None);
        let (_, value, detail) = bare.options().remove(2);
        assert_eq!(value, "off");
        assert_eq!(detail, "No skills found on this machine");
    }

    /// The project row opens the picker on the current default, so entering
    /// and confirming changes nothing.
    #[test]
    fn the_project_picker_opens_on_the_current_default() {
        let mut s = settings();
        s.cursor = 1;
        assert_eq!(s.select(), Action::Redraw);
        assert_eq!(s.pick, Some(0), "devtools is the default");
        let outcome = saved(s.select());
        assert_eq!(outcome.project.unwrap().project_id, "p1");
        assert_eq!(s.pick, None, "the picker closed");
    }

    /// Choosing another project saves it; "decide later" clears the default.
    #[test]
    fn picking_and_clearing_the_default_project() {
        let mut s = settings();
        s.cursor = 1;
        s.select();
        s.down();
        let outcome = saved(s.select());
        assert_eq!(outcome.project.unwrap().project_name, "mono");

        s.select();
        s.pick = Some(s.picker_options().len() - 1);
        let outcome = saved(s.select());
        assert!(outcome.project.is_none(), "decide later clears it");
        let (_, value, _) = s.options().remove(1);
        assert_eq!(value, "not set");
    }

    /// Creating a project is the one slow step; it says so, and a failure
    /// stays on the picker rather than pretending it worked.
    #[test]
    fn a_failed_create_keeps_the_picker() {
        let mut s = settings();
        s.cursor = 1;
        s.select();
        s.pick = Some(s.projects.len());
        // The create row carries the workspace it would create in, so the
        // project never lands somewhere the card guessed at.
        assert_eq!(s.select(), Action::CreateProject("ws1".into()));
        assert!(s.busy.is_some());

        assert_eq!(s.project_created(Err("no permission".into())), None);
        assert!(s.busy.is_none());
        assert_eq!(s.error.as_deref(), Some("no permission"));
        assert!(s.pick.is_some(), "still on the picker");

        let outcome = s
            .project_created(Ok(ProjectOption {
                project_id: "new".into(),
                project_name: "Cloud Agents".into(),
                environment_id: "env".into(),
                environment_name: "production".into(),
            }))
            .expect("a create that stuck is a change to save");
        assert_eq!(outcome.project.unwrap().project_name, "Cloud Agents");
        assert_eq!(s.pick, None);
    }

    /// Escape is layered: out of the picker first, out of the card second.
    #[test]
    fn escape_walks_out() {
        let mut s = settings();
        s.cursor = 1;
        s.select();
        assert_eq!(s.back(), Action::Redraw);
        assert_eq!(s.pick, None);
        assert_eq!(s.back(), Action::Close);
    }

    /// The tabs row toggles in place and rides the save like every other
    /// value.
    #[test]
    fn the_tabs_row_toggles_and_saves() {
        let mut s = settings();
        s.cursor = 4;
        assert!(s.cycles());
        assert!(saved(s.right()).hide_tabs, "shown toggles to hidden");
        assert!(!saved(s.right()).hide_tabs, "and back");
    }

    /// The last row hands over to the wizard.
    #[test]
    fn the_setup_row_replays_the_flow() {
        let mut s = settings();
        s.cursor = 5;
        assert_eq!(s.select(), Action::RunSetup);
    }

    /// ←/→ on rows that do not cycle must not save anything: a no-op write
    /// would still be a disk write per keypress.
    #[test]
    fn arrows_are_inert_where_nothing_cycles() {
        let mut s = settings();
        s.cursor = 1;
        assert!(!s.cycles());
        assert_eq!(s.left(), Action::None);
        assert_eq!(s.right(), Action::Redraw, "→ opens the picker");
        s.back();
        s.cursor = 5;
        assert_eq!(s.left(), Action::None);
        assert_eq!(s.right(), Action::None);
    }
}
