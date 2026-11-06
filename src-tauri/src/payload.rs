use serde_json::{json, Value};

use crate::model::TaskKind;

/// Azure DevOps rejects titles longer than this.
const MAX_TITLE_CHARS: usize = 255;
const MAX_HOURS: f64 = 999.0;
const MAX_COMMENT_CHARS: usize = 10_000;

pub struct TaskSpec<'a> {
    pub kind: TaskKind,
    pub story_title: &'a str,
    pub area_path: &'a str,
    pub iteration_path: &'a str,
    pub parent_url: &'a str,
    pub hours: f64,
    pub assignee: Option<&'a str>,
}

pub fn task_title(kind: TaskKind, story_title: &str) -> String {
    format!("{} {}", kind.prefix(), story_title.trim()).chars().take(MAX_TITLE_CHARS).collect()
}

/// The kind a title belongs to and what is left of it after the kind's prefix.
fn split_title(title: &str) -> Option<(TaskKind, String)> {
    let title = title.trim_start().to_ascii_lowercase();
    [TaskKind::Dev, TaskKind::Test, TaskKind::Demo].into_iter().find_map(|kind| {
        let name = kind.label().to_ascii_lowercase();
        let rest = title.strip_prefix(&format!("[{name}]")).or_else(|| {
            title.strip_prefix(&name).filter(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
        })?;
        Some((kind, rest.to_string()))
    })
}

/// Which kind of item a title belongs to, going by Trio's `Dev - ` / `Test - ` / `Demo - ` prefix
/// (the older `[DEV]` style and a plain leading `Test` word count too, since people create these
/// by hand). Case and spacing are forgiven since people edit titles.
pub fn kind_from_title(title: &str) -> Option<TaskKind> {
    split_title(title).map(|(kind, _)| kind)
}

/// True for a title that says nothing beyond the kind (`Test`, `Dev -`, `[Demo]`). Those are renamed
/// when an item is adopted; a title that already says more is kept.
pub fn is_bare_title(title: &str) -> bool {
    split_title(title).is_some_and(|(_, rest)| rest.trim_matches(|c: char| c.is_whitespace() || "-:–—[]".contains(c)).is_empty())
}

/// An item with no estimate (blank or zero) is a placeholder somebody raised ahead of refinement;
/// Trio offers to fill it in. One with hours has been estimated already and is left alone.
pub fn has_estimate(hours: Option<f64>) -> bool {
    hours.is_some_and(|hours| hours > 0.0)
}

/// The kind of an existing Dev / Test / Demo item: it carries one of those title prefixes and has
/// not been removed. `None` for anything else.
pub fn existing_kind(title: &str, state: &str) -> Option<TaskKind> {
    if state.eq_ignore_ascii_case("Removed") { None } else { kind_from_title(title) }
}

/// Checks the entered hours and rounds them to two decimal places.
pub fn validate_hours(hours: f64) -> Result<f64, String> {
    if !hours.is_finite() || hours <= 0.0 {
        return Err("Hours must be greater than zero.".into());
    }
    if hours > MAX_HOURS {
        return Err(format!("Hours must be {MAX_HOURS} or less."));
    }
    Ok((hours * 100.0).round() / 100.0)
}

/// Turns what the user typed into the HTML that Azure DevOps stores for a Discussion comment:
/// everything is escaped, and line breaks are kept.
pub fn comment_html(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Write a comment first.".into());
    }
    if text.chars().count() > MAX_COMMENT_CHARS {
        return Err(format!("Comments can be at most {MAX_COMMENT_CHARS} characters."));
    }
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    Ok(escaped.replace("\r\n", "\n").replace('\n', "<br>"))
}

/// Hours as people write them: `4`, `1.5`, `0.5` (no trailing zeros).
pub fn format_hours(hours: f64) -> String {
    let rounded = (hours * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 { format!("{rounded:.0}") } else { format!("{rounded}") }
}

/// The Discussion comment Trio leaves on the story when it creates items: the total, each item's
/// hours (always Dev, Test, Demo in that order) and when the estimate was made.
///
/// ```text
/// Trio Estimated Total : 4 Hrs
///
/// Dev : 2 hrs
/// Test : 1.5 hrs
/// Demo : 0.5 hrs
///
/// 30 Oct 2026, 14:05
/// ```
pub fn estimate_comment_html(items: &[(TaskKind, f64)], when: &str) -> String {
    let mut ordered: Vec<(TaskKind, f64)> = items.to_vec();
    ordered.sort_by_key(|(kind, _)| kind.order());
    let total: f64 = ordered.iter().map(|(_, hours)| hours).sum();

    let mut html = format!("<div><b>Trio Estimated Total : {} Hrs</b></div><div><br></div>", format_hours(total));
    for (kind, hours) in &ordered {
        html.push_str(&format!("<div>{} : {} hrs</div>", kind.label(), format_hours(*hours)));
    }
    html.push_str(&format!("<div><br></div><div>{when}</div>"));
    html
}

/// JSON Patch that adds a Discussion comment. Writing `System.History` is how the work item
/// form's Discussion box works, and unlike the dedicated comments endpoint it is not preview.
pub fn build_comment_patch(html: &str) -> Value {
    json!([{ "op": "add", "path": "/fields/System.History", "value": html }])
}

/// Builds the JSON Patch document for `POST .../workitems/${type}` (a `Task` or a `Test Case`,
/// see [`TaskKind::work_item_type`]).
///
/// Area and iteration are copied from the parent: an item created without them lands on the
/// project root and never appears on the sprint board.
pub fn build_task_patch(spec: &TaskSpec) -> Value {
    let add = |path: &str, value: Value| json!({ "op": "add", "path": path, "value": value });

    let mut ops = vec![
        add("/fields/System.Title", json!(task_title(spec.kind, spec.story_title))),
        add("/fields/System.AreaPath", json!(spec.area_path)),
        add("/fields/System.IterationPath", json!(spec.iteration_path)),
        add("/fields/Microsoft.VSTS.Scheduling.OriginalEstimate", json!(spec.hours)),
        add("/fields/Microsoft.VSTS.Scheduling.RemainingWork", json!(spec.hours)),
        add("/fields/Microsoft.VSTS.Scheduling.CompletedWork", json!(0)),
    ];

    if let Some(activity) = spec.kind.activity() {
        ops.push(add("/fields/Microsoft.VSTS.Common.Activity", json!(activity)));
    }

    if let Some(state) = spec.kind.initial_state() {
        ops.push(add("/fields/System.State", json!(state)));
    }

    if let Some(assignee) = spec.assignee.map(str::trim).filter(|a| !a.is_empty()) {
        ops.push(add("/fields/System.AssignedTo", json!(assignee)));
    }

    // Tasks and Test Cases both hang off the story as its children.
    ops.push(add("/relations/-", json!({ "rel": "System.LinkTypes.Hierarchy-Reverse", "url": spec.parent_url })));

    Value::Array(ops)
}

/// JSON Patch that fills in an item somebody created by hand: it gets the estimate, and Trio's
/// title when `rename` is set (the old title said nothing beyond the kind). The state, area,
/// iteration and links are left as they are, and so is the assignee unless one is given.
pub fn build_adopt_patch(kind: TaskKind, story_title: &str, hours: f64, assignee: Option<&str>, rename: bool) -> Value {
    let add = |path: &str, value: Value| json!({ "op": "add", "path": path, "value": value });

    let mut ops = Vec::new();
    if rename {
        ops.push(add("/fields/System.Title", json!(task_title(kind, story_title))));
    }
    ops.push(add("/fields/Microsoft.VSTS.Scheduling.OriginalEstimate", json!(hours)));
    ops.push(add("/fields/Microsoft.VSTS.Scheduling.RemainingWork", json!(hours)));
    if let Some(assignee) = assignee.map(str::trim).filter(|a| !a.is_empty()) {
        ops.push(add("/fields/System.AssignedTo", json!(assignee)));
    }
    Value::Array(ops)
}

/// Escapes a value for use inside a single-quoted WIQL string literal.
pub fn escape_wiql(value: &str) -> String {
    value.replace('\'', "''")
}

/// Items of the given work item types that are New or Active in the iteration, in backlog order.
pub fn wiql_for_iteration(iteration_path: &str, work_item_types: &[&str]) -> String {
    let types = work_item_types.iter().map(|t| format!("'{}'", escape_wiql(t))).collect::<Vec<_>>().join(", ");
    format!(
        "SELECT [System.Id] FROM WorkItems \
         WHERE [System.TeamProject] = @project \
         AND [System.WorkItemType] IN ({types}) \
         AND [System.State] IN ('New', 'Active') \
         AND [System.IterationPath] = '{}' \
         ORDER BY [Microsoft.VSTS.Common.BacklogPriority] ASC, [System.Id] ASC",
        escape_wiql(iteration_path)
    )
}

/// Body of the "reorder sprint backlog" call: put `id` after `previous_id` and before `next_id`
/// (`0` means "no neighbour on that side").
pub fn build_reorder_body(iteration_path: &str, id: u32, previous_id: Option<u32>, next_id: Option<u32>) -> Value {
    json!({
        "ids": [id],
        "iterationPath": iteration_path,
        "previousId": previous_id.unwrap_or(0),
        "nextId": next_id.unwrap_or(0),
    })
}

/// Moves `id` so it sits between the two neighbours, like the reorder call does. Returns false
/// when `id` is not in `order` or a neighbour is missing, leaving `order` untouched.
pub fn apply_move(order: &mut Vec<u32>, id: u32, previous_id: Option<u32>, next_id: Option<u32>) -> bool {
    let Some(from) = order.iter().position(|&x| x == id) else { return false };
    let mut moved = order.clone();
    moved.remove(from);
    let to = match (previous_id, next_id) {
        (Some(previous), _) => moved.iter().position(|&x| x == previous).map(|p| p + 1),
        (None, Some(next)) => moved.iter().position(|&x| x == next),
        (None, None) => Some(0),
    };
    let Some(to) = to else { return false };
    moved.insert(to, id);
    *order = moved;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_matched_to_their_kind_by_prefix() {
        assert_eq!(kind_from_title("Dev - Export the report"), Some(TaskKind::Dev));
        assert_eq!(kind_from_title("  test - export"), Some(TaskKind::Test));
        assert_eq!(kind_from_title("DEMO-sign off"), Some(TaskKind::Demo));
        assert_eq!(kind_from_title("[DEV] Export the report"), Some(TaskKind::Dev));
        assert_eq!(kind_from_title("[Test] Export"), Some(TaskKind::Test));
        // Only a prefix counts, and a longer word is not the kind.
        assert_eq!(kind_from_title("Development - plan"), None);
        assert_eq!(kind_from_title("Fix the Dev - env"), None);
        assert_eq!(kind_from_title("Tester feedback"), None);
        assert_eq!(kind_from_title(""), None);
    }

    #[test]
    fn hand_made_titles_are_matched() {
        assert_eq!(kind_from_title("Test"), Some(TaskKind::Test));
        assert_eq!(kind_from_title("test: login flow"), Some(TaskKind::Test));
        assert_eq!(kind_from_title("Test case for export"), Some(TaskKind::Test));
        assert_eq!(kind_from_title("Dev"), Some(TaskKind::Dev));
        assert_eq!(kind_from_title("Testing the export"), None);
        assert_eq!(kind_from_title("Demonstration"), None);
    }

    #[test]
    fn only_titles_with_nothing_beyond_the_kind_are_bare() {
        for bare in ["Test", " test ", "Test -", "Dev:", "[Demo]", "Demo - "] {
            assert!(is_bare_title(bare), "{bare}");
        }
        for named in ["Test - Accounts insert", "Dev - plan", "Test case for export", "Fix the Dev", ""] {
            assert!(!is_bare_title(named), "{named}");
        }
    }

    #[test]
    fn blank_and_zero_hours_are_not_an_estimate() {
        assert!(!has_estimate(None));
        assert!(!has_estimate(Some(0.0)));
        assert!(has_estimate(Some(0.5)));
    }

    #[test]
    fn adopting_estimates_and_optionally_renames_but_leaves_state_and_assignee() {
        let patch = build_adopt_patch(TaskKind::Test, " Export  ", 1.5, None, true);
        assert_eq!(field(&patch, "/fields/System.Title"), Some(&json!("Test - Export")));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.OriginalEstimate"), Some(&json!(1.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.RemainingWork"), Some(&json!(1.5)));
        assert_eq!(field(&patch, "/fields/System.State"), None);
        assert_eq!(field(&patch, "/fields/System.AssignedTo"), None);

        let assigned = build_adopt_patch(TaskKind::Dev, "Export", 2.0, Some("a@x.test"), true);
        assert_eq!(field(&assigned, "/fields/System.AssignedTo"), Some(&json!("a@x.test")));

        let kept = build_adopt_patch(TaskKind::Test, "Export", 1.5, None, false);
        assert_eq!(field(&kept, "/fields/System.Title"), None);
        assert_eq!(field(&kept, "/fields/Microsoft.VSTS.Scheduling.OriginalEstimate"), Some(&json!(1.5)));
    }

    #[test]
    fn only_new_test_cases_start_in_design() {
        let state = |kind| field(&build_task_patch(&spec(kind, None)), "/fields/System.State").cloned();
        assert_eq!(state(TaskKind::Test), Some(json!("Design")));
        assert_eq!(state(TaskKind::Dev), None);
        assert_eq!(state(TaskKind::Demo), None);
    }

    #[test]
    fn task_titles_round_trip_through_kind_from_title() {
        for kind in [TaskKind::Dev, TaskKind::Test, TaskKind::Demo] {
            assert_eq!(kind_from_title(&task_title(kind, "Anything")), Some(kind));
        }
    }

    #[test]
    fn reorder_body_uses_zero_for_missing_neighbours() {
        assert_eq!(
            build_reorder_body("Proj\\Sprint 1", 7, Some(3), None),
            json!({ "ids": [7], "iterationPath": "Proj\\Sprint 1", "previousId": 3, "nextId": 0 })
        );
    }

    #[test]
    fn apply_move_places_the_story_between_its_neighbours() {
        let mut order = vec![1, 2, 3, 4];
        assert!(apply_move(&mut order, 3, Some(1), Some(2)));
        assert_eq!(order, [1, 3, 2, 4]);
        assert!(apply_move(&mut order, 1, Some(4), None));
        assert_eq!(order, [3, 2, 4, 1]);
        assert!(apply_move(&mut order, 1, None, Some(3)));
        assert_eq!(order, [1, 3, 2, 4]);
    }

    #[test]
    fn apply_move_rejects_unknown_ids_without_changing_the_order() {
        let mut order = vec![1, 2, 3];
        assert!(!apply_move(&mut order, 9, Some(1), None));
        assert!(!apply_move(&mut order, 1, Some(9), None));
        assert_eq!(order, [1, 2, 3]);
    }

    fn spec(kind: TaskKind, assignee: Option<&'static str>) -> TaskSpec<'static> {
        TaskSpec {
            kind,
            story_title: "Reset password",
            area_path: "Proj\\Team A",
            iteration_path: "Proj\\Sprint 7",
            parent_url: "https://dev.azure.com/org/_apis/wit/workItems/42",
            hours: 6.5,
            assignee,
        }
    }

    fn field<'a>(patch: &'a Value, path: &str) -> Option<&'a Value> {
        patch.as_array()?.iter().find(|op| op["path"] == path).map(|op| &op["value"])
    }

    #[test]
    fn patch_sets_estimate_remaining_and_zero_completed() {
        let patch = build_task_patch(&spec(TaskKind::Dev, Some("dev@example.com")));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.OriginalEstimate"), Some(&json!(6.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.RemainingWork"), Some(&json!(6.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.CompletedWork"), Some(&json!(0)));
    }

    #[test]
    fn patch_copies_area_and_iteration_from_parent() {
        let patch = build_task_patch(&spec(TaskKind::Test, None));
        assert_eq!(field(&patch, "/fields/System.AreaPath"), Some(&json!("Proj\\Team A")));
        assert_eq!(field(&patch, "/fields/System.IterationPath"), Some(&json!("Proj\\Sprint 7")));
    }

    #[test]
    fn patch_links_to_parent_with_hierarchy_reverse() {
        let patch = build_task_patch(&spec(TaskKind::Dev, None));
        let relation = field(&patch, "/relations/-").unwrap();
        assert_eq!(relation["rel"], "System.LinkTypes.Hierarchy-Reverse");
        assert_eq!(relation["url"], "https://dev.azure.com/org/_apis/wit/workItems/42");
    }

    #[test]
    fn dev_tasks_have_an_activity_but_test_cases_do_not() {
        let dev = build_task_patch(&spec(TaskKind::Dev, None));
        let test = build_task_patch(&spec(TaskKind::Test, None));
        assert_eq!(field(&dev, "/fields/Microsoft.VSTS.Common.Activity"), Some(&json!("Development")));
        assert!(field(&test, "/fields/Microsoft.VSTS.Common.Activity").is_none());
    }

    #[test]
    fn titles_use_the_dev_and_test_prefixes() {
        let dev = build_task_patch(&spec(TaskKind::Dev, None));
        let test = build_task_patch(&spec(TaskKind::Test, None));
        assert_eq!(field(&dev, "/fields/System.Title"), Some(&json!("Dev - Reset password")));
        assert_eq!(field(&test, "/fields/System.Title"), Some(&json!("Test - Reset password")));
    }

    #[test]
    fn test_cases_are_children_of_the_story_and_carry_the_estimate() {
        let patch = build_task_patch(&spec(TaskKind::Test, Some("tester@example.com")));
        let relation = field(&patch, "/relations/-").unwrap();
        assert_eq!(relation["rel"], "System.LinkTypes.Hierarchy-Reverse");
        assert_eq!(relation["url"], "https://dev.azure.com/org/_apis/wit/workItems/42");
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.OriginalEstimate"), Some(&json!(6.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.RemainingWork"), Some(&json!(6.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.CompletedWork"), Some(&json!(0)));
        assert_eq!(field(&patch, "/fields/System.AssignedTo"), Some(&json!("tester@example.com")));
    }

    #[test]
    fn hours_are_written_without_trailing_zeros() {
        assert_eq!(format_hours(4.0), "4");
        assert_eq!(format_hours(1.5), "1.5");
        assert_eq!(format_hours(0.5), "0.5");
        assert_eq!(format_hours(2.456), "2.46");
        assert_eq!(format_hours(0.1 + 0.2), "0.3");
    }

    #[test]
    fn estimate_comment_lists_the_total_each_item_and_the_time() {
        // Given out of order, and totalling 4 hours.
        let html = estimate_comment_html(
            &[(TaskKind::Demo, 0.5), (TaskKind::Dev, 2.0), (TaskKind::Test, 1.5)],
            "30 Oct 2026, 14:05",
        );
        assert_eq!(
            html,
            "<div><b>Trio Estimated Total : 4 Hrs</b></div><div><br></div>\
             <div>Dev : 2 hrs</div><div>Test : 1.5 hrs</div><div>Demo : 0.5 hrs</div>\
             <div><br></div><div>30 Oct 2026, 14:05</div>"
        );
    }

    #[test]
    fn estimate_comment_only_lists_what_was_created() {
        let html = estimate_comment_html(&[(TaskKind::Test, 3.0)], "now");
        assert!(html.contains("Total : 3 Hrs"));
        assert!(html.contains("Test : 3 hrs"));
        assert!(!html.contains("Dev") && !html.contains("Demo"));
    }

    #[test]
    fn work_item_types_per_kind() {
        assert_eq!(TaskKind::Dev.work_item_type(), "Task");
        assert_eq!(TaskKind::Test.work_item_type(), "Test Case");
        assert_eq!(TaskKind::Demo.work_item_type(), "Task");
    }

    #[test]
    fn demo_is_a_task_with_the_demo_prefix_testing_activity_and_the_entered_hours() {
        let patch = build_task_patch(&TaskSpec { hours: 0.5, ..spec(TaskKind::Demo, Some("tester@example.com")) });
        assert_eq!(field(&patch, "/fields/System.Title"), Some(&json!("Demo - Reset password")));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Common.Activity"), Some(&json!("Testing")));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.OriginalEstimate"), Some(&json!(0.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.RemainingWork"), Some(&json!(0.5)));
        assert_eq!(field(&patch, "/fields/Microsoft.VSTS.Scheduling.CompletedWork"), Some(&json!(0)));
        assert_eq!(field(&patch, "/relations/-").unwrap()["rel"], "System.LinkTypes.Hierarchy-Reverse");
    }

    #[test]
    fn patch_omits_assignee_when_blank() {
        assert!(field(&build_task_patch(&spec(TaskKind::Dev, None)), "/fields/System.AssignedTo").is_none());
        assert!(field(&build_task_patch(&spec(TaskKind::Dev, Some("  "))), "/fields/System.AssignedTo").is_none());
        let with = build_task_patch(&spec(TaskKind::Dev, Some(" dev@example.com ")));
        assert_eq!(field(&with, "/fields/System.AssignedTo"), Some(&json!("dev@example.com")));
    }

    #[test]
    fn title_is_truncated_to_255_characters() {
        let long = "x".repeat(400);
        let title = task_title(TaskKind::Test, &long);
        assert_eq!(title.chars().count(), 255);
        assert!(title.starts_with("Test - xxx"));
    }

    #[test]
    fn hours_are_validated_and_rounded() {
        assert_eq!(validate_hours(2.456), Ok(2.46));
        assert!(validate_hours(0.0).is_err());
        assert!(validate_hours(-1.0).is_err());
        assert!(validate_hours(f64::NAN).is_err());
        assert!(validate_hours(f64::INFINITY).is_err());
        assert!(validate_hours(1000.0).is_err());
    }

    #[test]
    fn comment_html_escapes_markup_and_keeps_line_breaks() {
        assert_eq!(
            comment_html("  Is <b>this</b> & that OK?\r\nSecond \"line\"\n\nThird  ").unwrap(),
            "Is &lt;b&gt;this&lt;/b&gt; &amp; that OK?<br>Second &quot;line&quot;<br><br>Third"
        );
    }

    #[test]
    fn comment_html_rejects_empty_and_oversized_text() {
        assert!(comment_html("  \n ").is_err());
        assert!(comment_html(&"x".repeat(MAX_COMMENT_CHARS)).is_ok());
        assert!(comment_html(&"x".repeat(MAX_COMMENT_CHARS + 1)).is_err());
    }

    #[test]
    fn comment_patch_sets_system_history() {
        assert_eq!(
            build_comment_patch("hi"),
            json!([{ "op": "add", "path": "/fields/System.History", "value": "hi" }])
        );
    }

    #[test]
    fn wiql_escapes_single_quotes_in_iteration_path() {
        let query = wiql_for_iteration("Proj\\Dev's Sprint", &["User Story"]);
        assert!(query.contains("[System.IterationPath] = 'Proj\\Dev''s Sprint'"));
        assert!(query.contains("'New', 'Active'"));
    }

    #[test]
    fn wiql_lists_every_requested_work_item_type() {
        let query = wiql_for_iteration("Proj\\Sprint 1", &["User Story", "Technical Task"]);
        assert!(query.contains("[System.WorkItemType] IN ('User Story', 'Technical Task')"));
    }
}
