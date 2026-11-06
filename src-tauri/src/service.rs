use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::future::join_all;

use crate::backend::Backend;
use crate::html;
use crate::model::{
    Comment, CreateOutcome, CreateRequest, CreatedTask, ExistingItem, RawStory, StoryDetail, TaskInput, TaskKind, TaskResult,
};
use crate::payload::{self, TaskSpec};

const MAX_IMAGES_PER_FIELD: usize = 20;

pub async fn story_detail(backend: &Backend, id: u32) -> Result<StoryDetail, String> {
    let (raw, comments) = futures::join!(backend.story(id), story_comments(backend, id));
    let raw = raw?;
    let description = html::markdown_to_html_if_markdown(&raw.description);
    let acceptance = html::markdown_to_html_if_markdown(&raw.acceptance_criteria);
    let (description_html, acceptance_html, existing) = futures::join!(
        render_html(backend, &description),
        render_html(backend, &acceptance),
        existing_items(backend, &raw.child_ids)
    );

    // The comments API is a preview one; if it fails the story should still open.
    let (comments, comments_error) = match comments {
        Ok(comments) => (comments, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    Ok(StoryDetail {
        summary: raw.summary,
        description_html,
        acceptance_html,
        area_path: raw.area_path,
        iteration_path: raw.iteration_path,
        web_url: backend.web_url(id),
        comments,
        comments_error,
        existing,
    })
}

/// The Dev / Test / Demo items already under the story, found by their title prefix. Removed
/// items do not count, and if a kind appears twice the oldest one wins. This is a convenience:
/// when the children cannot be read the story still opens, as if nothing existed yet.
async fn existing_items(backend: &Backend, child_ids: &[u32]) -> Vec<ExistingItem> {
    if child_ids.is_empty() {
        return Vec::new();
    }
    let mut children = match backend.children(child_ids).await {
        Ok(children) => children,
        Err(error) => {
            eprintln!("Could not read the story's child items ({error}); treating none as existing.");
            return Vec::new();
        }
    };
    children.sort_by_key(|child| child.id);

    let mut found: Vec<ExistingItem> = Vec::new();
    for child in children {
        let Some(kind) = payload::existing_kind(&child.title, &child.state) else { continue };
        if found.iter().any(|item| item.kind == kind) {
            continue;
        }
        found.push(ExistingItem {
            kind,
            id: child.id,
            url: backend.web_url(child.id),
            adoptable: !payload::has_estimate(child.hours),
            work_item_type: child.work_item_type,
            title: child.title,
            state: child.state,
            hours: child.hours,
        });
    }
    found
}

/// The story's Discussion comments, oldest first, with their HTML sanitised.
pub async fn story_comments(backend: &Backend, id: u32) -> Result<Vec<Comment>, String> {
    let raw = backend.comments(id).await?;
    Ok(join_all(raw.into_iter().map(|comment| async move {
        Comment {
            id: comment.id,
            author: comment.author,
            created: comment.created,
            html: render_html(backend, &comment.html).await,
        }
    }))
    .await)
}

/// Sanitises the HTML and inlines its screenshots (see [`crate::html`]).
async fn render_html(backend: &Backend, raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    let clean = html::sanitize(raw);

    let fetches = html::image_srcs(&clean)
        .into_iter()
        .filter(|src| backend.can_fetch_image(src))
        .take(MAX_IMAGES_PER_FIELD)
        .map(|src| async move {
            let result = backend.fetch_image(&src).await;
            (src, result)
        });

    let mut inlined = HashMap::new();
    for (src, result) in join_all(fetches).await {
        match result {
            Ok((mime, bytes)) => {
                inlined.insert(src, format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
            }
            Err(error) => eprintln!("Skipping image {src}: {error}"),
        }
    }
    html::inline_images(&clean, &inlined)
}

/// Creates the requested child tasks one at a time. There are no transactions in Azure DevOps,
/// so each task reports its own outcome and the caller can retry only the ones that failed.
pub async fn create_tasks(backend: &Backend, request: CreateRequest) -> Result<CreateOutcome, String> {
    if request.tasks.is_empty() {
        return Err("Nothing to create.".into());
    }
    let mut kinds = HashSet::new();
    if !request.tasks.iter().all(|task| kinds.insert(task.kind)) {
        return Err("Each kind of task can only be requested once.".into());
    }

    let story = backend.story(request.story_id).await?;
    let parent_url = backend.parent_url(request.story_id);
    let adoptable = adoptable_ids(backend, &story, &request.tasks).await?;

    let mut results = Vec::with_capacity(request.tasks.len());
    let mut estimated = Vec::new();
    for task in request.tasks {
        let hours = match payload::validate_hours(task.hours) {
            Ok(hours) => hours,
            Err(error) => {
                results.push(TaskResult::failure(task.kind, error));
                continue;
            }
        };
        if let Some(id) = task.adopt_id {
            let result = match adoptable.get(&id) {
                Some((kind, rename)) if *kind == task.kind => {
                    let patch =
                        payload::build_adopt_patch(task.kind, &story.summary.title, hours, task.assignee.as_deref(), *rename);
                    backend.update_story(id, &patch).await.map(|()| CreatedTask { id, url: backend.web_url(id) })
                }
                _ => Err(format!("#{id} is not a {} item under this story.", task.kind.label())),
            };
            results.push(match result {
                Ok(item) => {
                    estimated.push((task.kind, hours));
                    TaskResult { adopted: true, ..TaskResult::success(task.kind, item) }
                }
                Err(error) => TaskResult::failure(task.kind, error),
            });
            continue;
        }
        let patch = payload::build_task_patch(&TaskSpec {
            kind: task.kind,
            story_title: &story.summary.title,
            area_path: &story.area_path,
            iteration_path: &story.iteration_path,
            parent_url: &parent_url,
            hours,
            assignee: task.assignee.as_deref(),
        });
        results.push(match backend.create_task(task.kind.work_item_type(), &patch).await {
            Ok(created) => {
                estimated.push((task.kind, hours));
                TaskResult::success(task.kind, created)
            }
            Err(error) => TaskResult::failure(task.kind, error),
        });
    }

    // Leave the estimate on the story. The items already exist, so a failure here is reported
    // alongside the results instead of turning the whole call into an error.
    let comment_error = if estimated.is_empty() {
        None
    } else {
        let when = chrono::Local::now().format("%d %b %Y, %H:%M").to_string();
        post_estimate_comment(backend, request.story_id, &estimated, &when).await.err()
    };
    Ok(CreateOutcome { results, comment_error })
}

/// The items among `tasks` that are to be adopted, with the kind each really is and whether its
/// title is bare (so it gets renamed). The ids come
/// from the UI, so they are checked against the story's own (not removed) children, and an item
/// that already has an estimate is never touched.
async fn adoptable_ids(backend: &Backend, story: &RawStory, tasks: &[TaskInput]) -> Result<HashMap<u32, (TaskKind, bool)>, String> {
    let wanted: Vec<u32> = tasks.iter().filter_map(|task| task.adopt_id).filter(|id| story.child_ids.contains(id)).collect();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(backend
        .children(&wanted)
        .await?
        .into_iter()
        .filter(|child| !payload::has_estimate(child.hours))
        .filter_map(|child| {
            let kind = payload::existing_kind(&child.title, &child.state)?;
            Some((child.id, (kind, payload::is_bare_title(&child.title))))
        })
        .collect())
}

/// Adds the "Trio Estimated Total" comment (see [`payload::estimate_comment_html`]) to the story.
async fn post_estimate_comment(
    backend: &Backend,
    story_id: u32,
    estimated: &[(TaskKind, f64)],
    when: &str,
) -> Result<(), String> {
    let html = payload::estimate_comment_html(estimated, when);
    backend.update_story(story_id, &payload::build_comment_patch(&html)).await
}

/// Adds a Discussion comment to the story.
pub async fn add_comment(backend: &Backend, story_id: u32, text: &str) -> Result<(), String> {
    let html = payload::comment_html(text)?;
    backend.update_story(story_id, &payload::build_comment_patch(&html)).await
}

/// Moves a story within its sprint so it sits between the two given neighbours.
pub async fn move_story(
    backend: &Backend,
    iteration_id: &str,
    iteration_path: &str,
    id: u32,
    previous_id: Option<u32>,
    next_id: Option<u32>,
) -> Result<(), String> {
    if iteration_id.trim().is_empty() || iteration_path.trim().is_empty() {
        return Err("Pick a sprint before reordering stories.".into());
    }
    if previous_id == Some(id) || next_id == Some(id) {
        return Err("A story cannot be moved next to itself.".into());
    }
    backend.move_story(iteration_id, iteration_path, id, previous_id, next_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::Demo;
    use crate::model::{TaskInput, TaskKind};

    #[tokio::test]
    async fn moved_stories_keep_their_new_position() {
        let backend = demo();
        let ids = |stories: Vec<crate::model::StorySummary>| stories.into_iter().map(|s| s.id).collect::<Vec<_>>();
        let before = ids(backend.stories("it-42", "Demo\\Sprint 42").await.unwrap());
        assert_eq!(before[..3], [1001, 1002, 1003]);

        move_story(&backend, "it-42", "Demo\\Sprint 42", 1003, None, Some(1001)).await.unwrap();
        let after = ids(backend.stories("it-42", "Demo\\Sprint 42").await.unwrap());
        assert_eq!(after[..3], [1003, 1001, 1002]);
        assert_eq!(after.len(), before.len());
    }

    #[tokio::test]
    async fn technical_tasks_are_listed_with_their_kind() {
        let backend = demo();
        let stories = backend.stories("it-42", "Demo\\Sprint 42").await.unwrap();
        let kind = |id| stories.iter().find(|s| s.id == id).unwrap().kind;
        assert_eq!(kind(1007), crate::model::StoryKind::Technical);
        assert_eq!(kind(1001), crate::model::StoryKind::Story);
        // They open and can be broken down like stories.
        assert_eq!(story_detail(&backend, 1007).await.unwrap().summary.kind, crate::model::StoryKind::Technical);
    }

    #[tokio::test]
    async fn bad_moves_are_rejected() {
        let backend = demo();
        assert!(move_story(&backend, "it-42", "Demo\\Sprint 42", 1001, Some(1001), None).await.is_err());
        assert!(move_story(&backend, "", "Demo\\Sprint 42", 1001, None, None).await.is_err());
        assert!(move_story(&backend, "it-42", "Demo\\Sprint 42", 9999, Some(1001), None).await.is_err());
    }

    fn demo() -> Backend {
        Backend::Demo(Demo::new())
    }

    fn input(kind: TaskKind, hours: f64) -> TaskInput {
        TaskInput { kind, hours, assignee: Some("dev.patel@demo.test".into()), adopt_id: None }
    }

    #[tokio::test]
    async fn story_detail_inlines_screenshots_and_leaves_no_remote_urls() {
        let detail = story_detail(&demo(), 1001).await.unwrap();
        assert!(detail.description_html.contains("data:image/svg+xml;base64,"));
        assert!(!detail.description_html.contains("dev.azure.com/demo/_apis"));
        assert!(detail.acceptance_html.contains("<li>"));
        assert_eq!(detail.summary.id, 1001);
    }

    #[tokio::test]
    async fn existing_dev_and_test_items_are_found_and_removed_ones_ignored() {
        let detail = story_detail(&demo(), 1002).await.unwrap();
        let found: Vec<_> = detail.existing.iter().map(|item| (item.kind, item.id, item.hours)).collect();
        assert_eq!(found, [(TaskKind::Dev, 9101, Some(3.0)), (TaskKind::Test, 9102, Some(1.5))]);
        assert_eq!(detail.existing[1].work_item_type, "Test Case");
        assert!(detail.existing[0].url.ends_with("/9101"));
        // Nothing under the other stories.
        assert!(story_detail(&demo(), 1001).await.unwrap().existing.is_empty());
    }

    #[tokio::test]
    async fn hand_made_items_are_offered_for_adoption_and_trio_ones_are_not() {
        let adoptable = |detail: &StoryDetail| detail.existing.iter().map(|i| (i.id, i.adoptable)).collect::<Vec<_>>();
        assert_eq!(adoptable(&story_detail(&demo(), 1003).await.unwrap()), [(9201, true), (9202, true), (9203, false)]);
        assert_eq!(adoptable(&story_detail(&demo(), 1002).await.unwrap()), [(9101, false), (9102, false)]);
    }

    #[tokio::test]
    async fn adopting_updates_the_existing_item_instead_of_creating_one() {
        let adopt = |kind, id| TaskInput { adopt_id: Some(id), ..input(kind, 2.0) };
        let request = CreateRequest { story_id: 1003, tasks: vec![adopt(TaskKind::Dev, 9202), adopt(TaskKind::Test, 9201)] };
        let results = create_tasks(&demo(), request).await.unwrap().results;
        assert!(results.iter().all(|r| r.ok && r.adopted), "{results:?}");
        assert_eq!(results.iter().filter_map(|r| r.id).collect::<Vec<_>>(), [9202, 9201]);
    }

    #[tokio::test]
    async fn an_item_that_already_has_hours_is_never_adopted() {
        let estimated = TaskInput { adopt_id: Some(9203), ..input(TaskKind::Demo, 1.0) };
        let results = create_tasks(&demo(), CreateRequest { story_id: 1003, tasks: vec![estimated] }).await.unwrap().results;
        assert!(!results[0].ok && !results[0].adopted);
    }

    #[tokio::test]
    async fn items_that_are_not_the_stories_own_cannot_be_adopted() {
        let foreign = TaskInput { adopt_id: Some(9201), ..input(TaskKind::Test, 1.0) };
        let wrong_kind = TaskInput { adopt_id: Some(9201), ..input(TaskKind::Dev, 1.0) };
        // 9201 belongs to story 1003, not 1001; and it is a Test item, not a Dev one.
        let results = create_tasks(&demo(), CreateRequest { story_id: 1001, tasks: vec![foreign] }).await.unwrap().results;
        assert!(!results[0].ok);
        let results = create_tasks(&demo(), CreateRequest { story_id: 1003, tasks: vec![wrong_kind] }).await.unwrap().results;
        assert!(!results[0].ok);
    }

    #[tokio::test]
    async fn story_detail_handles_empty_fields() {
        let detail = story_detail(&demo(), 1006).await.unwrap();
        assert!(detail.description_html.is_empty());
        assert!(detail.acceptance_html.is_empty());
    }

    #[tokio::test]
    async fn creates_each_requested_task() {
        let request = CreateRequest {
            story_id: 1001,
            tasks: vec![input(TaskKind::Dev, 8.0), input(TaskKind::Test, 3.5), input(TaskKind::Demo, 0.5)],
        };
        let results = create_tasks(&demo(), request).await.unwrap().results;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.ok && r.id.is_some() && r.url.is_some()));
        assert_eq!(results.iter().map(|r| r.kind).collect::<Vec<_>>(), [TaskKind::Dev, TaskKind::Test, TaskKind::Demo]);
    }

    #[tokio::test]
    async fn one_invalid_task_does_not_block_the_other() {
        let request = CreateRequest { story_id: 1001, tasks: vec![input(TaskKind::Dev, 0.0), input(TaskKind::Test, 2.0)] };
        let results = create_tasks(&demo(), request).await.unwrap().results;
        assert!(!results[0].ok);
        assert!(results[0].error.as_deref().unwrap().contains("greater than zero"));
        assert!(results[1].ok);
    }

    #[tokio::test]
    async fn creating_items_leaves_an_estimate_comment_on_the_story() {
        let backend = demo();
        let request = CreateRequest {
            story_id: 1004,
            tasks: vec![input(TaskKind::Demo, 0.5), input(TaskKind::Dev, 2.0), input(TaskKind::Test, 1.5)],
        };
        let outcome = create_tasks(&backend, request).await.unwrap();
        assert!(outcome.comment_error.is_none());

        let comments = story_comments(&backend, 1004).await.unwrap();
        assert_eq!(comments.len(), 1);
        let html = &comments[0].html;
        assert!(html.contains("Trio Estimated Total : 4 Hrs"), "{html}");
        let (dev, test, demo) = (html.find("Dev : 2 hrs"), html.find("Test : 1.5 hrs"), html.find("Demo : 0.5 hrs"));
        assert!(dev.is_some() && test > dev && demo > test, "{html}");
    }

    #[tokio::test]
    async fn the_estimate_comment_only_counts_items_that_were_created() {
        let backend = demo();
        let request = CreateRequest { story_id: 1005, tasks: vec![input(TaskKind::Dev, 0.0), input(TaskKind::Test, 2.0)] };
        create_tasks(&backend, request).await.unwrap();
        let html = story_comments(&backend, 1005).await.unwrap().remove(0).html;
        assert!(html.contains("Total : 2 Hrs") && html.contains("Test : 2 hrs"), "{html}");
        assert!(!html.contains("Dev :"), "{html}");
    }

    #[tokio::test]
    async fn no_estimate_comment_when_nothing_was_created() {
        let backend = demo();
        let request = CreateRequest { story_id: 1005, tasks: vec![input(TaskKind::Dev, 0.0)] };
        let outcome = create_tasks(&backend, request).await.unwrap();
        assert!(!outcome.results[0].ok && outcome.comment_error.is_none());
        assert!(story_comments(&backend, 1005).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_empty_and_duplicate_requests() {
        let empty = CreateRequest { story_id: 1001, tasks: vec![] };
        assert!(create_tasks(&demo(), empty).await.is_err());

        let duplicate = CreateRequest { story_id: 1001, tasks: vec![input(TaskKind::Dev, 1.0), input(TaskKind::Dev, 2.0)] };
        assert!(create_tasks(&demo(), duplicate).await.is_err());
    }

    #[tokio::test]
    async fn story_detail_includes_existing_comments_oldest_first() {
        let detail = story_detail(&demo(), 1001).await.unwrap();
        assert_eq!(detail.comments.len(), 2);
        assert_eq!(detail.comments[0].author, "Ben Whitfield");
        assert!(detail.comments[1].html.contains("identity provider"));
        assert!(detail.comments_error.is_none());
        assert!(story_detail(&demo(), 1006).await.unwrap().comments.is_empty());
    }

    #[tokio::test]
    async fn posted_comments_appear_in_the_story_comments() {
        let backend = demo();
        add_comment(&backend, 1004, "Can we ship this behind a flag?\nIt <touches> checkout.").await.unwrap();
        let comments = story_comments(&backend, 1004).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].html, "Can we ship this behind a flag?<br>It &lt;touches&gt; checkout.");
        // Other stories are unaffected.
        assert!(story_comments(&backend, 1002).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn comments_must_not_be_blank() {
        assert!(add_comment(&demo(), 1001, "Can we split this story?").await.is_ok());
        assert!(add_comment(&demo(), 1001, "   ").await.is_err());
    }

    #[tokio::test]
    async fn unknown_story_is_an_error() {
        let request = CreateRequest { story_id: 424242, tasks: vec![input(TaskKind::Dev, 1.0)] };
        assert!(create_tasks(&demo(), request).await.is_err());
    }
}
