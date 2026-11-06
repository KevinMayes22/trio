use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Iteration {
    pub id: String,
    pub name: String,
    pub path: String,
    /// "past", "current" or "future", as reported by Azure DevOps.
    pub time_frame: String,
    pub start_date: Option<String>,
    pub finish_date: Option<String>,
}

/// Work item type name of technical tasks in the team's process.
pub const TECHNICAL_TASK_TYPE: &str = "Technical Task";
pub const USER_STORY_TYPE: &str = "User Story";

/// What the sprint item is, which decides its icon in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StoryKind {
    Story,
    Technical,
}

impl StoryKind {
    pub fn from_work_item_type(work_item_type: &str) -> Self {
        if work_item_type.eq_ignore_ascii_case(TECHNICAL_TASK_TYPE) {
            StoryKind::Technical
        } else {
            StoryKind::Story
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorySummary {
    pub id: u32,
    pub kind: StoryKind,
    pub title: String,
    pub state: String,
    pub story_points: Option<f64>,
    pub assigned_to: Option<String>,
    pub tags: Vec<String>,
    /// The kinds of Dev / Test / Demo child item it already has (the ticks in the list).
    pub items: Vec<TaskKind>,
}

/// A story as returned by Azure DevOps, before its HTML has been sanitised.
#[derive(Debug, Clone, PartialEq)]
pub struct RawStory {
    pub summary: StorySummary,
    pub description: String,
    pub acceptance_criteria: String,
    pub area_path: String,
    pub iteration_path: String,
    /// Ids of the work items linked below this one (its children).
    pub child_ids: Vec<u32>,
}

/// A child work item as returned by Azure DevOps.
#[derive(Debug, Clone, PartialEq)]
pub struct RawChild {
    pub id: u32,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
    pub hours: Option<f64>,
}

/// A Dev / Test / Demo item that already exists under the story in Azure DevOps.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistingItem {
    pub kind: TaskKind,
    pub id: u32,
    pub url: String,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
    /// Original Estimate, if the item has one.
    pub hours: Option<f64>,
    /// No estimate yet (blank or zero): a placeholder raised ahead of refinement, which Trio offers
    /// to rename and estimate. Items with hours count as done and are left alone.
    pub adoptable: bool,
}

/// A Discussion comment as returned by Azure DevOps (`html` not yet sanitised).
#[derive(Debug, Clone, PartialEq)]
pub struct RawComment {
    pub id: u64,
    pub author: String,
    pub created: Option<String>,
    pub html: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: u64,
    pub author: String,
    /// ISO-8601; missing for comments just posted in demo mode.
    pub created: Option<String>,
    pub html: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryDetail {
    #[serde(flatten)]
    pub summary: StorySummary,
    pub description_html: String,
    pub acceptance_html: String,
    pub area_path: String,
    pub iteration_path: String,
    pub web_url: String,
    pub comments: Vec<Comment>,
    /// Set when the comments could not be loaded; the story itself still shows.
    pub comments_error: Option<String>,
    /// Dev / Test / Demo items already under the story, so the UI can show them as done.
    pub existing: Vec<ExistingItem>,
}

/// A team of the project; its sprints, members and board ordering are what Trio shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub display_name: String,
    /// Sign-in name / email; this is what `System.AssignedTo` accepts.
    pub unique_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Dev,
    Test,
    /// The demo / sign-off task the tester raises alongside the test case.
    Demo,
}

impl TaskKind {
    /// Short name used in the estimate comment.
    pub fn label(self) -> &'static str {
        match self {
            TaskKind::Dev => "Dev",
            TaskKind::Test => "Test",
            TaskKind::Demo => "Demo",
        }
    }

    /// Position in Dev, Test, Demo order.
    pub fn order(self) -> u8 {
        match self {
            TaskKind::Dev => 0,
            TaskKind::Test => 1,
            TaskKind::Demo => 2,
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            TaskKind::Dev => "Dev -",
            TaskKind::Test => "Test -",
            TaskKind::Demo => "Demo -",
        }
    }

    /// Dev work and the demo are `Task`s; the test work is a `Test Case`.
    pub fn work_item_type(self) -> &'static str {
        match self {
            TaskKind::Dev | TaskKind::Demo => "Task",
            TaskKind::Test => "Test Case",
        }
    }

    /// State a new item is created in. Test Cases start in `Design`; Tasks take the process default.
    pub fn initial_state(self) -> Option<&'static str> {
        match self {
            TaskKind::Test => Some("Design"),
            TaskKind::Dev | TaskKind::Demo => None,
        }
    }

    /// Value of `Microsoft.VSTS.Common.Activity`. Test Cases do not have an Activity field.
    pub fn activity(self) -> Option<&'static str> {
        match self {
            TaskKind::Dev => Some("Development"),
            TaskKind::Test => None,
            TaskKind::Demo => Some("Testing"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub kind: TaskKind,
    pub hours: f64,
    pub assignee: Option<String>,
    /// An existing item under the story to rename and estimate instead of creating a new one.
    #[serde(default)]
    pub adopt_id: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    pub story_id: u32,
    pub tasks: Vec<TaskInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedTask {
    pub id: u32,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResult {
    pub kind: TaskKind,
    pub ok: bool,
    pub id: Option<u32>,
    pub url: Option<String>,
    /// True when an existing item was updated instead of a new one created.
    pub adopted: bool,
    pub error: Option<String>,
}

/// What `create_tasks` reports back: each item's result, and whether the estimate comment
/// on the story could be posted (items that were created stay created either way).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOutcome {
    pub results: Vec<TaskResult>,
    pub comment_error: Option<String>,
}

impl TaskResult {
    pub fn success(kind: TaskKind, created: CreatedTask) -> Self {
        Self { kind, ok: true, id: Some(created.id), url: Some(created.url), adopted: false, error: None }
    }

    pub fn failure(kind: TaskKind, error: impl Into<String>) -> Self {
        Self { kind, ok: false, id: None, url: None, adopted: false, error: Some(error.into()) }
    }
}
