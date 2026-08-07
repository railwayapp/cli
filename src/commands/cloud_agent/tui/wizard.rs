//! First-run setup, inside the TUI.
//!
//! The same questions `railway ca setup` asks, in the same order, but as a
//! sequence of cards in the TUI's own style rather than a stack of inquire
//! prompts. It runs when there are no preferences yet, which is exactly the
//! moment someone has no idea what a "target" or a "harness" is, so each step
//! carries a line saying what the choice does.
//!
//! The project step reads from the tree the TUI already loaded, so choosing an
//! existing project costs no network. Creating one does, and that is the only
//! step that can fail.

use super::app::WorkspaceNode;
use super::theme::{THEMES, Theme};

/// One question in the flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The modal that opens it: set up now, or not.
    Intro,
    Project,
    /// Only when "use an existing project" was chosen.
    ProjectPick,
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
}

pub struct Wizard {
    pub step: Step,
    pub cursor: usize,
    /// Set while a step is doing something slow, e.g. creating a project.
    pub busy: Option<String>,
    /// Shown under the card when a step went wrong.
    pub error: Option<String>,
    pub project: Option<ProjectOption>,
    pub projects: Vec<ProjectOption>,
    pub agent: usize,
    pub skills: bool,
    pub skills_source: Option<String>,
    pub theme: usize,
}

/// What a keypress asked the loop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Redraw only; the theme step previews as the cursor moves.
    Redraw,
    /// Create the default project, then call [`Wizard::project_created`].
    CreateProject,
    /// The user finished; save these.
    Finish(Box<Outcome>),
    /// The user declined or backed out of the first question.
    Cancel,
}

impl Wizard {
    /// Build the flow, taking the project list from the loaded tree.
    pub fn new(
        tree: &[WorkspaceNode],
        harness: Option<&str>,
        theme: &Theme,
        skills_source: Option<String>,
    ) -> Self {
        let projects = tree
            .iter()
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
            .collect();
        Self {
            step: Step::Intro,
            cursor: 0,
            busy: None,
            error: None,
            project: None,
            projects,
            agent: harness
                .and_then(|h| super::app::HARNESSES.iter().position(|x| *x == h))
                .unwrap_or(0),
            skills: skills_source.is_some(),
            skills_source,
            theme: theme.index(),
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
            Step::Project => {
                let mut options = vec![(
                    "Create a project".into(),
                    "A new Railway project named \"Cloud Agents\" to keep them in".into(),
                )];
                if !self.projects.is_empty() {
                    options.push((
                        "Use an existing project".into(),
                        format!("Choose one of your {} projects", self.projects.len()),
                    ));
                }
                options.push((
                    "Decide later".into(),
                    "Pick a target each time you launch".into(),
                ));
                options
            }
            // The environment rides in the label. As a description it was the
            // same sentence on every row, which is a lot of words to say
            // "production" a dozen times.
            Step::ProjectPick => self
                .projects
                .iter()
                .map(|p| {
                    (
                        format!("{} ({})", p.project_name, p.environment_name),
                        String::new(),
                    )
                })
                .collect(),
            Step::Agent => super::app::HARNESSES
                .iter()
                .map(|slug| {
                    let what = match *slug {
                        "claude" => "Anthropic's Claude Code",
                        "codex" => "OpenAI's Codex",
                        _ => "xAI's Grok",
                    };
                    ((*slug).to_string(), what.to_string())
                })
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
        self.step = Step::Project;
        self.cursor = 0;
    }

    pub fn title(&self) -> &'static str {
        match self.step {
            Step::Intro => "Set up cloud agents?",
            Step::Project => "Where should agents live?",
            Step::ProjectPick => "Which project?",
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
            Step::Project | Step::ProjectPick => 0,
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
                self.go(Step::Project);
                Action::Redraw
            }
            Step::Project => {
                let has_existing = !self.projects.is_empty();
                match (self.cursor, has_existing) {
                    (0, _) => {
                        self.busy = Some("Creating Cloud Agents…".into());
                        Action::CreateProject
                    }
                    (1, true) => {
                        self.go(Step::ProjectPick);
                        Action::Redraw
                    }
                    _ => {
                        self.project = None;
                        self.go(Step::Agent);
                        Action::Redraw
                    }
                }
            }
            Step::ProjectPick => {
                self.project = self.projects.get(self.cursor).cloned();
                self.go(Step::Agent);
                Action::Redraw
            }
            Step::Agent => {
                self.agent = self.cursor.min(super::app::HARNESSES.len() - 1);
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
                    agent: super::app::HARNESSES[self.agent].to_string(),
                    skills: self.skills,
                    skills_source: self.skills_source.clone(),
                    theme: THEMES[self.theme].slug.to_string(),
                }))
            }
        }
    }

    /// Escape: back a step, or out of the flow from the first one.
    pub fn back(&mut self) -> Action {
        match self.step {
            Step::Intro => Action::Cancel,
            Step::Project => {
                self.go(Step::Intro);
                Action::Redraw
            }
            Step::ProjectPick => {
                self.go(Step::Project);
                Action::Redraw
            }
            Step::Agent => {
                self.go(Step::Project);
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

    /// The project step finished, one way or the other.
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
        Wizard::new(&tree(), Some("codex"), Theme::default_theme(), None)
    }

    #[test]
    fn declining_the_intro_leaves_without_saving() {
        let mut w = wizard();
        w.down();
        assert_eq!(w.select(), Action::Cancel);
    }

    /// The whole flow, ending in the preferences it collected.
    #[test]
    fn the_flow_collects_every_answer() {
        let mut w = wizard();
        assert_eq!(w.select(), Action::Redraw); // set up now
        assert_eq!(w.step, Step::Project);

        w.down(); // use an existing project
        assert_eq!(w.select(), Action::Redraw);
        assert_eq!(w.step, Step::ProjectPick);
        w.down(); // mono
        w.select();
        assert_eq!(w.step, Step::Agent);
        assert_eq!(w.project.as_ref().unwrap().project_name, "mono");
        // The project's first environment comes along with it.
        assert_eq!(w.project.as_ref().unwrap().environment_name, "staging");

        // The agent step opens on the harness already configured.
        assert_eq!(w.cursor, 1, "codex was passed in");
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
        w.select();
        assert_eq!(w.select(), Action::CreateProject);
        assert!(w.busy.is_some());

        w.project_created(Err("no permission".into()));
        assert_eq!(w.step, Step::Project);
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
        w.select(); // create → busy, but the step is what matters here
        w.step = Step::Agent;
        assert_eq!(w.back(), Action::Redraw);
        assert_eq!(w.step, Step::Project);
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

    /// The project rows carry their environment in the label, so the list is
    /// not a column of identical sentences.
    #[test]
    fn project_rows_name_their_environment_inline() {
        let mut w = wizard();
        w.step = Step::ProjectPick;
        let options = w.options();
        assert_eq!(options[0].0, "devtools (production)");
        assert_eq!(options[1].0, "mono (staging)");
        assert!(
            options.iter().all(|(_, what)| what.is_empty()),
            "no repeated description line"
        );
    }

    /// Chosen from the menu, setup starts at the first question: "do you want
    /// to set up?" has already been answered by choosing it.
    #[test]
    fn skipping_the_intro_starts_at_the_first_question() {
        let mut w = wizard();
        w.skip_intro();
        assert_eq!(w.step, Step::Project);
        assert_eq!(w.position(), Some((0, 4)));

        // And escape from there goes back to the intro rather than out, so the
        // flow is still walkable in both directions.
        assert_eq!(w.back(), Action::Redraw);
        assert_eq!(w.step, Step::Intro);
    }

    /// With no projects loaded there is nothing to pick, so the option is not
    /// offered at all.
    #[test]
    fn the_pick_option_is_hidden_without_projects() {
        let mut w = Wizard::new(&[], None, Theme::default_theme(), None);
        w.select();
        let labels: Vec<String> = w.options().into_iter().map(|(label, _)| label).collect();
        assert_eq!(labels, vec!["Create a project", "Decide later"]);

        // And "decide later" still lands on the next question.
        w.down();
        w.select();
        assert_eq!(w.step, Step::Agent);
        assert!(w.project.is_none());
    }
}
