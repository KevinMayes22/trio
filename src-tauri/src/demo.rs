//! Offline stand-in for Azure DevOps, so the app can be tried (and developed) without a PAT.
//! Created tasks are only logged to stderr, never persisted.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use serde_json::Value;

use crate::model::{CreatedTask, Iteration, Member, RawChild, RawComment, RawStory, StoryKind, StorySummary, TaskKind, Team};
use crate::payload;

const DEMO_IMAGE_PREFIX: &str = "https://dev.azure.com/demo/_apis/wit/attachments/";
const AREA_PATH: &str = "Demo\\Web Team";

pub struct Demo {
    next_task_id: AtomicU32,
    /// Comments posted this session, so they show up in the list like they would for real.
    posted_comments: Mutex<Vec<(u32, RawComment)>>,
    /// Story order per sprint after the user has moved stories around.
    orders: Mutex<HashMap<String, Vec<u32>>>,
}

struct DemoStory {
    id: u32,
    title: &'static str,
    points: Option<f64>,
    assigned_to: Option<&'static str>,
    iteration: &'static str,
    tags: &'static [&'static str],
    description: String,
    acceptance: String,
}

impl Demo {
    pub fn new() -> Self {
        Self { next_task_id: AtomicU32::new(9001), posted_comments: Mutex::new(Vec::new()), orders: Mutex::new(HashMap::new()) }
    }

    pub fn comments(&self, story_id: u32) -> Vec<RawComment> {
        let comment = |id, author: &str, created: &str, html: &str| RawComment {
            id,
            author: author.into(),
            created: Some(format!("{created}Z")),
            html: html.into(),
        };
        let mut comments = match story_id {
            1001 => vec![
                comment(1, "Ben Whitfield", "2026-10-27T09:14:00", "<div>Do we need to support SSO users here? They don't have a password to reset.</div>"),
                comment(
                    2,
                    "Amara Okafor",
                    "2026-10-27T10:02:00",
                    "<div>Good point. SSO users should see a message pointing them to their identity provider. I'll add it to the acceptance criteria.</div>",
                ),
            ],
            1003 => vec![comment(3, "Chloe Nguyen", "2026-10-28T15:40:00", "<div>The error styles are ready in the design file, so this should be quick to wire up.</div>")],
            _ => Vec::new(),
        };
        let posted = self.posted_comments.lock().expect("comments lock");
        comments.extend(posted.iter().filter(|(id, _)| *id == story_id).map(|(_, comment)| comment.clone()));
        comments
    }

    /// Two teams so the picker can be tried; the sample sprints are the same for both.
    pub fn teams(&self) -> Vec<Team> {
        [("team-1", "Demo Team"), ("team-2", "Platform Team")]
            .into_iter()
            .map(|(id, name)| Team { id: id.into(), name: name.into() })
            .collect()
    }

    pub fn iterations(&self) -> Vec<Iteration> {
        let iteration = |id: &str, name: &str, time_frame: &str, start: &str, finish: &str| Iteration {
            id: id.into(),
            name: name.into(),
            path: format!("Demo\\{name}"),
            time_frame: time_frame.into(),
            start_date: Some(format!("{start}T00:00:00Z")),
            finish_date: Some(format!("{finish}T00:00:00Z")),
        };
        vec![
            iteration("it-41", "Sprint 41", "past", "2026-10-12", "2026-10-23"),
            iteration("it-42", "Sprint 42", "current", "2026-10-26", "2026-11-06"),
            iteration("it-43", "Sprint 43", "future", "2026-11-09", "2026-11-20"),
        ]
    }

    pub fn stories(&self, iteration_path: &str) -> Vec<StorySummary> {
        let mut stories: Vec<_> =
            fixtures().into_iter().filter(|s| format!("Demo\\{}", s.iteration) == iteration_path).collect();
        if let Some(order) = self.orders.lock().expect("orders lock").get(iteration_path) {
            stories.sort_by_key(|s| order.iter().position(|&id| id == s.id).unwrap_or(usize::MAX));
        }
        stories.into_iter().map(summary).collect()
    }

    /// Reorders the sprint in memory only, mirroring what the real reorder call does.
    pub fn move_story(&self, iteration_path: &str, id: u32, previous_id: Option<u32>, next_id: Option<u32>) -> Result<(), String> {
        let mut order: Vec<u32> = self.stories(iteration_path).iter().map(|s| s.id).collect();
        if !payload::apply_move(&mut order, id, previous_id, next_id) {
            return Err(format!("Could not move story {id} in the demo sprint."));
        }
        self.orders.lock().expect("orders lock").insert(iteration_path.to_string(), order);
        Ok(())
    }

    pub fn story(&self, id: u32) -> Result<RawStory, String> {
        let story = fixtures().into_iter().find(|s| s.id == id).ok_or_else(|| format!("Demo story {id} not found."))?;
        Ok(RawStory {
            description: story.description.clone(),
            acceptance_criteria: story.acceptance.clone(),
            area_path: AREA_PATH.into(),
            iteration_path: format!("Demo\\{}", story.iteration),
            child_ids: match story.id {
                EXPORT_STORY_ID => EXISTING_CHILD_IDS.to_vec(),
                HAND_MADE_STORY_ID => HAND_MADE_CHILD_IDS.to_vec(),
                _ => Vec::new(),
            },
            summary: summary(story),
        })
    }

    /// Story 1002 already has Dev and Test items (and a removed Demo one that must be ignored).
    pub fn children(&self, ids: &[u32]) -> Vec<RawChild> {
        let child = |id, work_item_type: &str, title: &str, state: &str, hours| RawChild {
            id,
            work_item_type: work_item_type.into(),
            title: title.into(),
            state: state.into(),
            hours,
        };
        [
            child(9101, "Task", "Dev - Export the monthly report as CSV", "Active", Some(3.0)),
            child(9102, "Test Case", "Test - Export the monthly report as CSV", "Design", Some(1.5)),
            child(9103, "Task", "Demo - Export the monthly report as CSV", "Removed", Some(0.5)),
            child(9201, "Test Case", "Test", "Design", None),
            child(9202, "Task", "Dev - sketch the approach", "New", Some(0.0)),
            child(9203, "Task", "Demo", "New", Some(0.5)),
        ]
        .into_iter()
        .filter(|c| ids.contains(&c.id))
        .collect()
    }

    pub fn members(&self) -> Vec<Member> {
        [
            ("Amara Okafor", "amara.okafor@demo.test"),
            ("Ben Whitfield", "ben.whitfield@demo.test"),
            ("Chloe Nguyen", "chloe.nguyen@demo.test"),
            ("Dev Patel", "dev.patel@demo.test"),
            ("Elena Rossi", "elena.rossi@demo.test"),
            ("Farid Haddad", "farid.haddad@demo.test"),
            ("Grace Liu", "grace.liu@demo.test"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (name, email))| Member { id: format!("demo-{i}"), display_name: name.into(), unique_name: email.into() })
        .collect()
    }

    pub fn create_task(&self, work_item_type: &str, patch: &Value) -> CreatedTask {
        let id = self.next_task_id.fetch_add(1, Ordering::Relaxed);
        eprintln!("[demo] would create {work_item_type} #{id}: {patch}");
        CreatedTask { id, url: self.web_url(id) }
    }

    pub fn update_story(&self, id: u32, patch: &Value) {
        eprintln!("[demo] would update story #{id}: {patch}");
        let history = patch.as_array().and_then(|ops| ops.iter().find(|op| op["path"] == "/fields/System.History"));
        if let Some(html) = history.and_then(|op| op["value"].as_str()) {
            let mut posted = self.posted_comments.lock().expect("comments lock");
            let comment_id = 5000 + posted.len() as u64;
            posted.push((id, RawComment { id: comment_id, author: "You (demo)".into(), created: None, html: html.into() }));
        }
    }

    pub fn web_url(&self, id: u32) -> String {
        format!("https://dev.azure.com/demo/Demo/_workitems/edit/{id}")
    }

    pub fn parent_url(&self, id: u32) -> String {
        format!("https://dev.azure.com/demo/_apis/wit/workItems/{id}")
    }

    pub fn can_fetch_image(&self, url: &str) -> bool {
        url.starts_with(DEMO_IMAGE_PREFIX)
    }

    pub fn fetch_image(&self, url: &str) -> Result<(String, Vec<u8>), String> {
        let caption = url.strip_prefix(DEMO_IMAGE_PREFIX).ok_or("Not a demo attachment.")?;
        Ok(("image/svg+xml".into(), mock_screenshot(caption.split('?').next().unwrap_or(caption)).into_bytes()))
    }
}

const EXPORT_STORY_ID: u32 = 1002;
const EXISTING_CHILD_IDS: [u32; 3] = [9101, 9102, 9103];
/// Story 1003 has a Test Case and a Dev task somebody created by hand, without hours, plus a
/// hand-made Demo task that was already estimated.
const HAND_MADE_STORY_ID: u32 = 1003;
const HAND_MADE_CHILD_IDS: [u32; 3] = [9201, 9202, 9203];

/// Demo items that are technical tasks rather than user stories.
const TECHNICAL_IDS: [u32; 2] = [1007, 1008];

fn summary(story: DemoStory) -> StorySummary {
    StorySummary {
        id: story.id,
        kind: if TECHNICAL_IDS.contains(&story.id) { StoryKind::Technical } else { StoryKind::Story },
        title: story.title.into(),
        state: if story.id % 2 == 0 { "Active" } else { "New" }.into(),
        story_points: story.points,
        assigned_to: story.assigned_to.map(str::to_string),
        tags: story.tags.iter().map(|t| t.to_string()).collect(),
        items: match story.id {
            EXPORT_STORY_ID => vec![TaskKind::Dev, TaskKind::Test],
            _ => Vec::new(),
        },
    }
}

fn image_tag(name: &str, alt: &str) -> String {
    format!(r#"<img src="{DEMO_IMAGE_PREFIX}{name}?fileName={name}.png" alt="{alt}">"#)
}

fn fixtures() -> Vec<DemoStory> {
    vec![
        DemoStory {
            id: 1001,
            title: "Password reset by email link",
            points: Some(5.0),
            assigned_to: None,
            iteration: "Sprint 42",
            tags: &["auth", "customer-facing"],
            description: format!(
                "<div>As a <b>customer</b> who has forgotten my password, I want to request a reset link by email \
                 so that I can regain access without contacting support.</div>\
                 <div><br></div><div>Today the sign-in page shows a dead end after three failed attempts:</div>{}\
                 <div><br></div><div>Design notes are in the <a href=\"https://example.com/design\">design doc</a>. \
                 The link must expire after <i>30 minutes</i>.</div>",
                image_tag("sign-in-locked", "Sign-in page after three failed attempts")
            ),
            acceptance: "<ul><li>A \"Forgot password?\" link appears on the sign-in page</li>\
                         <li>Submitting a known email sends a single-use reset link</li>\
                         <li>Unknown emails get the same confirmation message (no account enumeration)</li>\
                         <li>Links expire after 30 minutes and cannot be reused</li></ul>"
                .into(),
        },
        DemoStory {
            id: 1002,
            title: "Export the monthly report as CSV",
            points: Some(3.0),
            assigned_to: Some("Dev Patel"),
            iteration: "Sprint 42",
            tags: &["reporting"],
            description: "<div>Finance need the monthly report in a spreadsheet-friendly format. \
                          Add an <b>Export CSV</b> button next to the existing PDF export.</div>"
                .into(),
            acceptance: "<ul><li>CSV has one row per transaction with a header row</li>\
                         <li>Dates are ISO-8601, amounts use a dot decimal separator</li>\
                         <li>Exports of 50,000 rows complete in under 10 seconds</li></ul>"
                .into(),
        },
        DemoStory {
            id: 1003,
            title: "Show validation errors inline on the checkout form",
            points: Some(8.0),
            assigned_to: None,
            iteration: "Sprint 42",
            tags: &["checkout", "accessibility"],
            description: format!(
                "<div>Errors currently appear in a banner at the top of the page, away from the field that caused them:</div>{}\
                 <div><br></div><div>Show each message next to its field and move focus to the first invalid one on submit.</div>",
                image_tag("checkout-banner-error", "Checkout form with a banner error")
            ),
            acceptance: "<ul><li>Each invalid field shows its own message beneath it</li>\
                         <li>Focus moves to the first invalid field on submit</li>\
                         <li>Messages are announced to screen readers</li></ul>"
                .into(),
        },
        DemoStory {
            id: 1004,
            title: "Remember the last selected filter",
            points: Some(2.0),
            assigned_to: None,
            iteration: "Sprint 42",
            tags: &[],
            description: "<div>Users re-apply the same filter every visit. Persist the last selection per user.</div>".into(),
            acceptance: String::new(),
        },
        DemoStory {
            id: 1005,
            title: "Audit log for permission changes",
            points: Some(13.0),
            assigned_to: Some("Grace Liu"),
            iteration: "Sprint 42",
            tags: &["security", "compliance"],
            description: "<div>Compliance require a tamper-evident record of who changed whose permissions, and when.</div>".into(),
            acceptance: "<ul><li>Every grant and revoke is recorded with actor, target, old and new role</li>\
                         <li>Entries cannot be edited or deleted through the UI</li></ul>"
                .into(),
        },
        DemoStory {
            id: 1006,
            title: "Dark mode for the dashboard",
            points: None,
            assigned_to: None,
            iteration: "Sprint 42",
            tags: &["ui"],
            description: String::new(),
            acceptance: String::new(),
        },
        DemoStory {
            id: 1007,
            title: "Upgrade the logging library to v4",
            points: None,
            assigned_to: None,
            iteration: "Sprint 42",
            tags: &["tech-debt"],
            description: "<div>v3 is end-of-life and logs secrets in debug mode. Upgrade to v4 and adopt its structured output.</div>".into(),
            acceptance: "<ul><li>All services build against v4</li><li>No secrets appear in debug logs</li></ul>".into(),
        },
        DemoStory {
            id: 1008,
            title: "Add a database index for the monthly report query",
            points: None,
            assigned_to: Some("Ben Whitfield"),
            iteration: "Sprint 42",
            tags: &["performance"],
            description: "<div>The report query scans the whole transactions table. Add a composite index on account and date.</div>".into(),
            acceptance: String::new(),
        },
        DemoStory {
            id: 1101,
            title: "Bulk-assign tickets to a queue",
            points: Some(5.0),
            assigned_to: None,
            iteration: "Sprint 43",
            tags: &["support"],
            description: "<div>Support leads want to select several tickets and move them to a queue in one action.</div>".into(),
            acceptance: "<ul><li>Select up to 100 tickets</li><li>Partial failures are reported per ticket</li></ul>".into(),
        },
    ]
}

/// A simple placeholder "screenshot" so the image pipeline can be seen working.
fn mock_screenshot(caption: &str) -> String {
    let caption = caption.replace('-', " ").replace('&', "&amp;").replace('<', "&lt;");
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="720" height="360" viewBox="0 0 720 360">
<rect width="720" height="360" fill="#f3f4f6"/>
<rect x="0" y="0" width="720" height="36" fill="#e5e7eb"/>
<circle cx="20" cy="18" r="6" fill="#f87171"/><circle cx="40" cy="18" r="6" fill="#fbbf24"/><circle cx="60" cy="18" r="6" fill="#34d399"/>
<rect x="200" y="70" width="320" height="230" rx="10" fill="#fff" stroke="#d1d5db"/>
<rect x="224" y="96" width="140" height="14" rx="4" fill="#9ca3af"/>
<rect x="224" y="128" width="272" height="30" rx="5" fill="#fff" stroke="#d1d5db"/>
<rect x="224" y="170" width="272" height="30" rx="5" fill="#fff" stroke="#f87171"/>
<rect x="224" y="208" width="200" height="10" rx="3" fill="#fca5a5"/>
<rect x="224" y="240" width="272" height="34" rx="6" fill="#3b82f6"/>
<text x="360" y="332" font-family="Segoe UI, sans-serif" font-size="15" fill="#6b7280" text-anchor="middle">{caption}</text>
</svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_sprint_has_stories_with_and_without_content() {
        let demo = Demo::new();
        let stories = demo.stories("Demo\\Sprint 42");
        assert!(stories.len() >= 5);
        assert!(demo.story(1006).unwrap().description.is_empty());
        assert!(demo.story(1001).unwrap().description.contains("<img"));
    }

    #[test]
    fn created_tasks_get_increasing_ids() {
        let demo = Demo::new();
        let a = demo.create_task("Task", &Value::Null).id;
        let b = demo.create_task("Task", &Value::Null).id;
        assert_eq!(b, a + 1);
    }

    #[test]
    fn serves_only_demo_attachments() {
        let demo = Demo::new();
        assert!(demo.fetch_image(&format!("{DEMO_IMAGE_PREFIX}sign-in-locked?fileName=x.png")).is_ok());
        assert!(demo.fetch_image("https://example.com/x.png").is_err());
    }
}
