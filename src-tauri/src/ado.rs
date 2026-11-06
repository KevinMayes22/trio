//! Azure DevOps REST client (api-version 7.1, PAT authentication).

use std::collections::HashMap;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::{header, Client, RequestBuilder, Response, StatusCode};
use serde_json::{json, Value};
use url::Url;

use crate::model::{
    CreatedTask, Iteration, Member, RawChild, RawComment, RawStory, StoryKind, StorySummary, TaskKind, Team,
    TECHNICAL_TASK_TYPE, USER_STORY_TYPE,
};
use crate::payload;

const API_VERSION: &str = "7.1";
/// Reading comments is still a preview API in 7.1 (adding them goes through `System.History`).
const COMMENTS_API_VERSION: &str = "7.1-preview.4";
/// Fallback for `move_story` if 7.1 reports the reorder resource as a preview.
const REORDER_PREVIEW_API_VERSION: &str = "7.1-preview.1";
const MAX_COMMENTS: &str = "200";
const BATCH_SIZE: usize = 200;
const TEAMS_PAGE_SIZE: usize = 100;
const MAX_TEAM_PAGES: usize = 50;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

const SUMMARY_FIELDS: [&str; 8] = [
    "System.Id",
    "System.WorkItemType",
    "System.Title",
    "System.State",
    "System.AssignedTo",
    "System.Tags",
    "Microsoft.VSTS.Scheduling.StoryPoints",
    "System.IterationPath",
];

const CHILD_FIELDS: [&str; 5] =
    ["System.Id", "System.WorkItemType", "System.Title", "System.State", "Microsoft.VSTS.Scheduling.OriginalEstimate"];

pub struct AdoClient {
    http: Client,
    base: Url,
    org: String,
    project: String,
    team: String,
    auth: String,
}

impl AdoClient {
    pub fn production(org: &str, project: &str, team: &str, pat: &str) -> Result<Self, String> {
        let base = Url::parse("https://dev.azure.com").expect("static URL");
        Self::new(base, org, project, team, pat)
    }

    pub fn new(base: Url, org: &str, project: &str, team: &str, pat: &str) -> Result<Self, String> {
        let http = Client::builder()
            // A bad PAT makes Azure DevOps redirect to a sign-in page; never follow it.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Trio/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("Could not create HTTP client: {e}"))?;

        Ok(Self {
            http,
            base,
            org: org.to_string(),
            project: project.to_string(),
            team: team.to_string(),
            auth: format!("Basic {}", STANDARD.encode(format!(":{pat}"))),
        })
    }

    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Url {
        self.versioned_url(segments, query, API_VERSION)
    }

    fn versioned_url(&self, segments: &[&str], query: &[(&str, &str)], api_version: &str) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut().expect("base URL is hierarchical").pop_if_empty().extend(segments);
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
            pairs.append_pair("api-version", api_version);
        }
        url
    }

    pub fn web_url(&self, id: u32) -> String {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("base URL is hierarchical")
            .pop_if_empty()
            .extend([self.org.as_str(), self.project.as_str(), "_workitems", "edit", &id.to_string()]);
        url.to_string()
    }

    /// The URL a child's `Hierarchy-Reverse` relation points at.
    pub fn parent_url(&self, id: u32) -> String {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("base URL is hierarchical")
            .pop_if_empty()
            .extend([self.org.as_str(), "_apis", "wit", "workItems", &id.to_string()]);
        url.to_string()
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, String> {
        let response = request
            .header(header::AUTHORIZATION, &self.auth)
            .send()
            .await
            .map_err(|e| format!("Could not reach Azure DevOps: {e}"))?;

        let status = response.status();
        if status.is_redirection() || status == StatusCode::UNAUTHORIZED || status == StatusCode::NON_AUTHORITATIVE_INFORMATION {
            return Err("Azure DevOps did not accept the PAT. It may be wrong, expired, or for a different organisation.".into());
        }
        if status.is_success() {
            return Ok(response);
        }

        let message = error_message(&response.text().await.unwrap_or_default());
        if status == StatusCode::FORBIDDEN {
            return Err(format!(
                "The PAT is not allowed to do this ({message}). It needs Work Items (Read & write) and Project and Team (Read)."
            ));
        }
        Err(format!("Azure DevOps returned {status}: {message}"))
    }

    async fn json(&self, request: RequestBuilder) -> Result<Value, String> {
        self.send(request.header(header::ACCEPT, "application/json"))
            .await?
            .json::<Value>()
            .await
            .map_err(|e| format!("Unexpected response from Azure DevOps: {e}"))
    }

    /// The team the sprints, members and ordering belong to; an error until one has been chosen.
    fn team(&self) -> Result<&str, String> {
        if self.team.is_empty() { Err("Choose a team first.".into()) } else { Ok(&self.team) }
    }

    /// The teams of the project, by name. Needs no team, so it works before one has been chosen.
    pub async fn teams(&self) -> Result<Vec<Team>, String> {
        let mut teams = Vec::new();
        for page in 0..MAX_TEAM_PAGES {
            let skip = (page * TEAMS_PAGE_SIZE).to_string();
            let url = self.url(
                &[&self.org, "_apis", "projects", &self.project, "teams"],
                &[("$top", &TEAMS_PAGE_SIZE.to_string()), ("$skip", &skip)],
            );
            let found = parse_teams(&self.json(self.http.get(url)).await?);
            let full_page = found.len() >= TEAMS_PAGE_SIZE;
            teams.extend(found);
            if !full_page {
                break;
            }
        }
        teams.sort_by_key(|team| team.name.to_lowercase());
        Ok(teams)
    }

    pub async fn iterations(&self) -> Result<Vec<Iteration>, String> {
        let team = self.team()?;
        let url = self.url(&[&self.org, &self.project, team, "_apis", "work", "teamsettings", "iterations"], &[]);
        Ok(parse_iterations(&self.json(self.http.get(url)).await?))
    }

    /// The ids of the sprint's work items in sprint backlog order, which is the order of the sprint board.
    async fn sprint_order(&self, iteration_id: &str) -> Result<Vec<u32>, String> {
        let team = self.team()?;
        let url = self.url(
            &[&self.org, &self.project, team, "_apis", "work", "teamsettings", "iterations", iteration_id, "workitems"],
            &[],
        );
        Ok(parse_sprint_order(&self.json(self.http.get(url)).await?))
    }

    pub async fn stories(&self, iteration_id: &str, iteration_path: &str) -> Result<Vec<StorySummary>, String> {
        let query_ids = |types: &[&str]| {
            let url = self.url(&[&self.org, &self.project, "_apis", "wit", "wiql"], &[]);
            let body = json!({ "query": payload::wiql_for_iteration(iteration_path, types) });
            async move { Ok::<_, String>(parse_wiql_ids(&self.json(self.http.post(url).json(&body)).await?)) }
        };
        let ids = match query_ids(&[USER_STORY_TYPE, TECHNICAL_TASK_TYPE]).await {
            Ok(ids) => ids,
            // A process without a "Technical Task" type makes WIQL reject the whole query;
            // the stories must still show.
            Err(error) => {
                eprintln!("Listing stories with technical tasks failed ({error}); listing stories only.");
                query_ids(&[USER_STORY_TYPE]).await?
            }
        };
        // The board's own order wins; WIQL's `ORDER BY` only approximates it (the rank field differs
        // between process templates), so it is the fallback if the sprint order cannot be read.
        let ids = match self.sprint_order(iteration_id).await {
            Ok(board_order) => merge_order(&ids, &board_order),
            Err(error) => {
                eprintln!("Could not read the sprint board order ({error}); using the query order.");
                ids
            }
        };

        // WIQL only returns ids; fetch the fields in batches and keep the board order.
        let mut summaries = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH_SIZE) {
            let url = self.url(&[&self.org, &self.project, "_apis", "wit", "workitemsbatch"], &[]);
            let body = json!({ "ids": chunk, "fields": SUMMARY_FIELDS });
            let batch = self.json(self.http.post(url).json(&body)).await?;
            summaries.extend(batch["value"].as_array().into_iter().flatten().filter_map(parse_summary));
        }
        summaries.sort_by_key(|s| ids.iter().position(|id| *id == s.id).unwrap_or(usize::MAX));

        // The ticks in the list are a convenience; the stories must show even if they cannot be worked out.
        match self.story_items(&ids).await {
            Ok(mut items) => summaries.iter_mut().for_each(|s| s.items = items.remove(&s.id).unwrap_or_default()),
            Err(error) => eprintln!("Could not check which stories already have child items ({error})."),
        }
        Ok(summaries)
    }

    /// For each of `story_ids`, the kinds of (not removed, estimated) Dev / Test / Demo child it has, in Dev, Test, Demo order.
    async fn story_items(&self, story_ids: &[u32]) -> Result<HashMap<u32, Vec<TaskKind>>, String> {
        // `$expand` cannot be combined with `fields`, so the links come in a call of their own.
        let mut children_of: Vec<(u32, Vec<u32>)> = Vec::new();
        for chunk in story_ids.chunks(BATCH_SIZE) {
            let url = self.url(&[&self.org, &self.project, "_apis", "wit", "workitemsbatch"], &[]);
            let body = json!({ "ids": chunk, "$expand": "relations" });
            let batch = self.json(self.http.post(url).json(&body)).await?;
            for item in batch["value"].as_array().into_iter().flatten() {
                if let Some(id) = item["id"].as_u64() {
                    children_of.push((id as u32, parse_child_ids(item)));
                }
            }
        }
        let mut all_children: Vec<u32> = children_of.iter().flat_map(|(_, children)| children.iter().copied()).collect();
        all_children.sort_unstable();
        all_children.dedup();
        if all_children.is_empty() {
            return Ok(HashMap::new());
        }
        let kinds: HashMap<u32, TaskKind> = self
            .children(&all_children)
            .await?
            .into_iter()
            .filter(|child| payload::has_estimate(child.hours))
            .filter_map(|child| Some((child.id, payload::existing_kind(&child.title, &child.state)?)))
            .collect();
        Ok(children_of
            .into_iter()
            .map(|(story, children)| {
                let mut found: Vec<TaskKind> = children.iter().filter_map(|id| kinds.get(id).copied()).collect();
                found.sort_by_key(|kind| kind.order());
                found.dedup();
                (story, found)
            })
            .filter(|(_, found)| !found.is_empty())
            .collect())
    }

    pub async fn story(&self, id: u32) -> Result<RawStory, String> {
        // `relations` carries the links to the story's children (Dev / Test / Demo items).
        let url = self.url(&[&self.org, &self.project, "_apis", "wit", "workitems", &id.to_string()], &[("$expand", "relations")]);
        let item = self.json(self.http.get(url)).await?;
        parse_story(&item).ok_or_else(|| format!("Work item {id} is not a readable story."))
    }

    /// The title, type, state and estimate of the given work items (the children of a story).
    pub async fn children(&self, ids: &[u32]) -> Result<Vec<RawChild>, String> {
        let mut children = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH_SIZE) {
            let url = self.url(&[&self.org, &self.project, "_apis", "wit", "workitemsbatch"], &[]);
            let body = json!({ "ids": chunk, "fields": CHILD_FIELDS });
            let batch = self.json(self.http.post(url).json(&body)).await?;
            children.extend(batch["value"].as_array().into_iter().flatten().filter_map(parse_child));
        }
        Ok(children)
    }

    pub async fn comments(&self, id: u32) -> Result<Vec<RawComment>, String> {
        let url = self.versioned_url(
            &[&self.org, &self.project, "_apis", "wit", "workItems", &id.to_string(), "comments"],
            &[("$top", MAX_COMMENTS)],
            COMMENTS_API_VERSION,
        );
        Ok(parse_comments(&self.json(self.http.get(url)).await?))
    }

    pub async fn members(&self) -> Result<Vec<Member>, String> {
        let team = self.team()?;
        let url = self.url(
            &[&self.org, "_apis", "projects", &self.project, "teams", team, "members"],
            &[("$top", "500")],
        );
        Ok(parse_members(&self.json(self.http.get(url)).await?))
    }

    /// Creates a work item of `work_item_type` ("Task", "Test Case", ...) from a JSON Patch.
    pub async fn create_task(&self, work_item_type: &str, patch: &Value) -> Result<CreatedTask, String> {
        let url = self.url(&[&self.org, &self.project, "_apis", "wit", "workitems", &format!("${work_item_type}")], &[]);
        let request = self
            .http
            .post(url)
            .header(header::CONTENT_TYPE, "application/json-patch+json")
            .body(patch.to_string());
        let created = self.json(request).await?;
        let id = created["id"].as_u64().ok_or("Azure DevOps did not return the new task's id.")? as u32;
        Ok(CreatedTask { id, url: self.web_url(id) })
    }

    /// Applies a JSON Patch to an existing work item (used to add a Discussion comment).
    pub async fn update_story(&self, id: u32, patch: &Value) -> Result<(), String> {
        let url = self.url(&[&self.org, &self.project, "_apis", "wit", "workitems", &id.to_string()], &[]);
        let request = self
            .http
            .patch(url)
            .header(header::CONTENT_TYPE, "application/json-patch+json")
            .body(patch.to_string());
        self.json(request).await.map(|_| ())
    }

    /// Moves a story within the sprint backlog (which is what the sprint board is ordered by).
    pub async fn move_story(
        &self,
        iteration_id: &str,
        iteration_path: &str,
        id: u32,
        previous_id: Option<u32>,
        next_id: Option<u32>,
    ) -> Result<(), String> {
        let body = payload::build_reorder_body(iteration_path, id, previous_id, next_id);
        let team = self.team()?;
        let segments = [&self.org, &self.project, team, "_apis", "work", "iterations", iteration_id, "workitemsorder"];
        let send = |api_version: &str| {
            let url = self.versioned_url(&segments, &[], api_version);
            self.json(self.http.patch(url).json(&body))
        };
        match send(API_VERSION).await {
            // Some organisations still serve this resource as a preview.
            Err(error) if error.contains("under preview") => send(REORDER_PREVIEW_API_VERSION).await.map(|_| ()),
            result => result.map(|_| ()),
        }
    }

    /// Whether `url` is an attachment this client may fetch with the PAT. Anything else
    /// must never receive the credential.
    pub fn can_fetch_image(&self, url: &str) -> bool {
        let Ok(url) = Url::parse(url) else { return false };
        let host = url.host_str().unwrap_or_default();
        let is_base = Some(host) == self.base.host_str() && url.scheme() == self.base.scheme();
        let trusted_host = is_base
            || (url.scheme() == "https" && (host == "dev.azure.com" || host.ends_with(".visualstudio.com")));
        trusted_host && url.path().contains("/_apis/wit/attachments/")
    }

    /// Returns the image's content type and bytes.
    pub async fn fetch_image(&self, url: &str) -> Result<(String, Vec<u8>), String> {
        if !self.can_fetch_image(url) {
            return Err("Not an Azure DevOps attachment.".into());
        }
        let response = self
            .http
            .get(url)
            .header(header::AUTHORIZATION, &self.auth)
            .send()
            .await
            .map_err(|e| format!("Could not fetch image: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("Image request returned {}.", response.status()));
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or_default().trim().to_ascii_lowercase())
            .unwrap_or_default();
        if !content_type.starts_with("image/") {
            return Err(format!("Attachment is {content_type:?}, not an image."));
        }

        let bytes = response.bytes().await.map_err(|e| format!("Could not read image: {e}"))?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("Image is too large to display.".into());
        }
        Ok((content_type, bytes.to_vec()))
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// `System.AssignedTo` is an identity object on current API versions and a string on old ones.
fn identity_name(value: &Value) -> Option<String> {
    value["displayName"].as_str().or_else(|| value.as_str()).map(str::to_string).filter(|n| !n.is_empty())
}

pub(crate) fn parse_iterations(response: &Value) -> Vec<Iteration> {
    let mut iterations: Vec<Iteration> = response["value"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| Iteration {
            id: text(&item["id"]),
            name: text(&item["name"]),
            path: text(&item["path"]),
            time_frame: item["attributes"]["timeFrame"].as_str().unwrap_or("future").to_string(),
            start_date: item["attributes"]["startDate"].as_str().map(str::to_string),
            finish_date: item["attributes"]["finishDate"].as_str().map(str::to_string),
        })
        .collect();
    // ISO-8601 dates sort lexically; sprints without dates go last.
    iterations.sort_by(|a, b| match (&a.start_date, &b.start_date) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    iterations
}

pub(crate) fn parse_wiql_ids(response: &Value) -> Vec<u32> {
    response["workItems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["id"].as_u64())
        .map(|id| id as u32)
        .collect()
}

/// Work item ids in the order the sprint backlog lists them (each id once; child tasks come
/// along too and are dropped later by [`merge_order`]).
pub(crate) fn parse_sprint_order(response: &Value) -> Vec<u32> {
    let mut seen = std::collections::HashSet::new();
    response["workItemRelations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|relation| relation["target"]["id"].as_u64())
        .map(|id| id as u32)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// `ids` in the order of `board_order`; ids the board does not mention keep their relative order at the end.
pub(crate) fn merge_order(ids: &[u32], board_order: &[u32]) -> Vec<u32> {
    let wanted: std::collections::HashSet<u32> = ids.iter().copied().collect();
    let mut merged: Vec<u32> = board_order.iter().copied().filter(|id| wanted.contains(id)).collect();
    let placed: std::collections::HashSet<u32> = merged.iter().copied().collect();
    merged.extend(ids.iter().copied().filter(|id| !placed.contains(id)));
    merged
}

pub(crate) fn parse_summary(item: &Value) -> Option<StorySummary> {
    let fields = &item["fields"];
    Some(StorySummary {
        id: item["id"].as_u64()? as u32,
        kind: StoryKind::from_work_item_type(&text(&fields["System.WorkItemType"])),
        title: text(&fields["System.Title"]),
        state: text(&fields["System.State"]),
        story_points: fields["Microsoft.VSTS.Scheduling.StoryPoints"].as_f64(),
        assigned_to: identity_name(&fields["System.AssignedTo"]),
        tags: text(&fields["System.Tags"])
            .split(';')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        items: Vec::new(),
    })
}

pub(crate) fn parse_story(item: &Value) -> Option<RawStory> {
    let fields = &item["fields"];
    Some(RawStory {
        summary: parse_summary(item)?,
        description: text(&fields["System.Description"]),
        acceptance_criteria: text(&fields["Microsoft.VSTS.Common.AcceptanceCriteria"]),
        area_path: text(&fields["System.AreaPath"]),
        iteration_path: text(&fields["System.IterationPath"]),
        child_ids: parse_child_ids(item),
    })
}

/// Ids of the work items linked below `item` (needs the item fetched with `$expand=relations`).
fn parse_child_ids(item: &Value) -> Vec<u32> {
    item["relations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|relation| relation["rel"] == "System.LinkTypes.Hierarchy-Forward")
        .filter_map(|relation| relation["url"].as_str().and_then(work_item_id_from_url))
        .collect()
}

/// The id at the end of a work item URL such as `https://dev.azure.com/org/_apis/wit/workItems/123`.
fn work_item_id_from_url(url: &str) -> Option<u32> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

pub(crate) fn parse_child(item: &Value) -> Option<RawChild> {
    let fields = &item["fields"];
    Some(RawChild {
        id: item["id"].as_u64()? as u32,
        work_item_type: text(&fields["System.WorkItemType"]),
        title: text(&fields["System.Title"]),
        state: text(&fields["System.State"]),
        hours: fields["Microsoft.VSTS.Scheduling.OriginalEstimate"].as_f64(),
    })
}

/// Oldest first, so the thread reads in conversation order. Deleted comments are dropped.
pub(crate) fn parse_comments(response: &Value) -> Vec<RawComment> {
    let mut comments: Vec<RawComment> = response["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|comment| comment["isDeleted"].as_bool() != Some(true))
        .filter_map(|comment| {
            Some(RawComment {
                id: comment["id"].as_u64()?,
                author: identity_name(&comment["createdBy"]).unwrap_or_else(|| "Unknown".to_string()),
                created: comment["createdDate"].as_str().map(str::to_string),
                html: text(&comment["text"]),
            })
        })
        .collect();
    comments.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
    comments
}

pub(crate) fn parse_teams(response: &Value) -> Vec<Team> {
    response["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|team| {
            let name = text(&team["name"]);
            if name.is_empty() { None } else { Some(Team { id: text(&team["id"]), name }) }
        })
        .collect()
}

pub(crate) fn parse_members(response: &Value) -> Vec<Member> {
    let mut members: Vec<Member> = response["value"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| &item["identity"])
        // The member list can include groups, which cannot be assigned work.
        .filter(|identity| identity["isContainer"].as_bool() != Some(true))
        .filter_map(|identity| {
            let unique_name = text(&identity["uniqueName"]);
            let display_name = text(&identity["displayName"]);
            if unique_name.is_empty() || display_name.is_empty() {
                return None;
            }
            Some(Member { id: text(&identity["id"]), display_name, unique_name })
        })
        .collect();
    members.sort_by_key(|m| m.display_name.to_lowercase());
    members
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> AdoClient {
        AdoClient::new(Url::parse(&server.uri()).unwrap(), "myorg", "My Project", "My Team", "secret-pat").unwrap()
    }

    #[test]
    fn parses_teams_and_skips_nameless_ones() {
        let teams = parse_teams(&json!({ "value": [
            { "id": "1", "name": "Platform" },
            { "id": "2", "name": "" },
            { "id": "3", "name": "Accounts" },
        ]}));
        assert_eq!(teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Platform", "Accounts"]);
    }

    #[tokio::test]
    async fn lists_the_projects_teams_by_name_without_needing_a_team() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/myorg/_apis/projects/My%20Project/teams"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": "1", "name": "platform" },
                { "id": "2", "name": "Accounts" },
            ]})))
            .mount(&server)
            .await;
        let no_team = AdoClient::new(Url::parse(&server.uri()).unwrap(), "myorg", "My Project", "", "pat").unwrap();
        let teams = no_team.teams().await.unwrap();
        assert_eq!(teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Accounts", "platform"]);
        // Anything that belongs to a team refuses until one is chosen.
        assert!(no_team.iterations().await.is_err());
        assert!(no_team.members().await.is_err());
    }

    #[test]
    fn parses_iterations_sorted_by_start_date() {
        let response = json!({ "value": [
            { "id": "b", "name": "Sprint 2", "path": "P\\Sprint 2", "attributes": { "timeFrame": "future", "startDate": "2026-11-09T00:00:00Z", "finishDate": "2026-11-20T00:00:00Z" } },
            { "id": "a", "name": "Sprint 1", "path": "P\\Sprint 1", "attributes": { "timeFrame": "current", "startDate": "2026-10-26T00:00:00Z", "finishDate": "2026-11-06T00:00:00Z" } },
            { "id": "c", "name": "Backlog", "path": "P\\Backlog", "attributes": { "timeFrame": "future" } },
        ]});
        let iterations = parse_iterations(&response);
        assert_eq!(iterations.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["Sprint 1", "Sprint 2", "Backlog"]);
        assert_eq!(iterations[0].time_frame, "current");
    }

    #[test]
    fn parses_story_summary_with_identity_and_tags() {
        let item = json!({ "id": 42, "fields": {
            "System.Title": "Reset password", "System.State": "Active", "System.WorkItemType": "Technical Task",
            "System.AssignedTo": { "displayName": "Ann Lee", "uniqueName": "ann@example.com" },
            "System.Tags": "ui; auth ;", "Microsoft.VSTS.Scheduling.StoryPoints": 5.0 } });
        let story = parse_summary(&item).unwrap();
        assert_eq!(story.id, 42);
        assert_eq!(story.assigned_to.as_deref(), Some("Ann Lee"));
        assert_eq!(story.tags, ["ui", "auth"]);
        assert_eq!(story.story_points, Some(5.0));
        assert_eq!(story.kind, StoryKind::Technical);
    }

    #[test]
    fn story_children_come_from_forward_hierarchy_links_only() {
        let item = json!({ "id": 7, "fields": { "System.Title": "x", "System.State": "New" }, "relations": [
            { "rel": "System.LinkTypes.Hierarchy-Forward", "url": "https://dev.azure.com/o/_apis/wit/workItems/71" },
            { "rel": "System.LinkTypes.Hierarchy-Reverse", "url": "https://dev.azure.com/o/_apis/wit/workItems/3" },
            { "rel": "System.LinkTypes.Related", "url": "https://dev.azure.com/o/_apis/wit/workItems/72" },
            { "rel": "AttachedFile", "url": "https://dev.azure.com/o/_apis/wit/attachments/abc" },
            { "rel": "System.LinkTypes.Hierarchy-Forward", "url": "https://dev.azure.com/o/_apis/wit/workItems/73/" },
        ]});
        assert_eq!(parse_story(&item).unwrap().child_ids, [71, 73]);
        let none = json!({ "id": 7, "fields": { "System.Title": "x" } });
        assert!(parse_story(&none).unwrap().child_ids.is_empty());
    }

    #[test]
    fn parses_a_child_with_its_estimate() {
        let item = json!({ "id": 9, "fields": { "System.WorkItemType": "Test Case", "System.Title": "Test - x",
            "System.State": "Design", "Microsoft.VSTS.Scheduling.OriginalEstimate": 1.5 } });
        let child = parse_child(&item).unwrap();
        assert_eq!((child.id, child.work_item_type.as_str(), child.hours), (9, "Test Case", Some(1.5)));
        let bare = json!({ "id": 10, "fields": { "System.Title": "Dev - x" } });
        assert_eq!(parse_child(&bare).unwrap().hours, None);
    }

    #[test]
    fn items_without_a_type_are_treated_as_stories() {
        let item = json!({ "id": 1, "fields": { "System.Title": "x", "System.State": "New" } });
        assert_eq!(parse_summary(&item).unwrap().kind, StoryKind::Story);
    }

    #[test]
    fn members_skip_groups_and_sort_by_name() {
        let response = json!({ "value": [
            { "identity": { "id": "1", "displayName": "Zed Zed", "uniqueName": "zed@example.com" } },
            { "identity": { "id": "2", "displayName": "[Proj]\\Team", "uniqueName": "vstfs:///Group", "isContainer": true } },
            { "identity": { "id": "3", "displayName": "amy Ames", "uniqueName": "amy@example.com" } },
        ]});
        let members = parse_members(&response);
        assert_eq!(members.iter().map(|m| m.unique_name.as_str()).collect::<Vec<_>>(), ["amy@example.com", "zed@example.com"]);
    }

    #[test]
    fn comments_are_sorted_oldest_first_and_skip_deleted() {
        let response = json!({ "totalCount": 3, "comments": [
            { "id": 12, "text": "<div>second</div>", "createdDate": "2026-10-28T09:00:00Z", "createdBy": { "displayName": "Ben Whitfield" } },
            { "id": 11, "text": "<div>first</div>", "createdDate": "2026-10-27T09:00:00Z", "createdBy": { "displayName": "Amara Okafor" } },
            { "id": 13, "text": "gone", "createdDate": "2026-10-29T09:00:00Z", "isDeleted": true, "createdBy": { "displayName": "Dev Patel" } },
        ]});
        let comments = parse_comments(&response);
        assert_eq!(comments.iter().map(|c| c.id).collect::<Vec<_>>(), [11, 12]);
        assert_eq!(comments[0].author, "Amara Okafor");
        assert_eq!(comments[0].html, "<div>first</div>");
    }

    #[test]
    fn comment_without_an_author_is_still_listed() {
        let comments = parse_comments(&json!({ "comments": [{ "id": 1, "text": "hi" }] }));
        assert_eq!(comments[0].author, "Unknown");
        assert_eq!(comments[0].created, None);
    }

    #[test]
    fn only_azure_devops_attachments_are_fetchable() {
        let client = AdoClient::production("o", "p", "t", "pat").unwrap();
        assert!(client.can_fetch_image("https://dev.azure.com/o/p/_apis/wit/attachments/abc?fileName=a.png"));
        assert!(client.can_fetch_image("https://o.visualstudio.com/p/_apis/wit/attachments/abc"));
        assert!(!client.can_fetch_image("https://evil.example/_apis/wit/attachments/abc"));
        assert!(!client.can_fetch_image("https://dev.azure.com/o/p/_apis/wit/workitems/1"));
        assert!(!client.can_fetch_image("http://dev.azure.com/o/p/_apis/wit/attachments/abc"));
        assert!(!client.can_fetch_image("not a url"));
    }

    #[test]
    fn builds_encoded_urls_and_links() {
        let client = AdoClient::production("myorg", "My Project", "My Team", "pat").unwrap();
        assert_eq!(client.web_url(7), "https://dev.azure.com/myorg/My%20Project/_workitems/edit/7");
        assert_eq!(client.parent_url(7), "https://dev.azure.com/myorg/_apis/wit/workItems/7");
    }

    #[tokio::test]
    async fn stories_run_wiql_then_batch_fetch_and_keep_backlog_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/wiql"))
            .and(query_param("api-version", "7.1"))
            .and(header_exists("authorization"))
            .and(body_partial_json(json!({ "query": payload::wiql_for_iteration("P\\Sprint 1", &["User Story", "Technical Task"]) })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "workItems": [{ "id": 9 }, { "id": 3 }] })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .and(body_partial_json(json!({ "$expand": "relations" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [{ "id": 3 }, { "id": 9 }] })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .and(body_partial_json(json!({ "ids": [9, 3] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": 3, "fields": { "System.Title": "Three", "System.State": "New", "System.WorkItemType": "User Story" } },
                { "id": 9, "fields": { "System.Title": "Nine", "System.State": "Active", "System.WorkItemType": "Technical Task" } },
            ]})))
            .expect(1)
            .mount(&server)
            .await;

        let stories = client(&server).stories("it-1", "P\\Sprint 1").await.unwrap();
        assert_eq!(stories.iter().map(|s| s.id).collect::<Vec<_>>(), [9, 3]);
        assert_eq!(stories.iter().map(|s| s.kind).collect::<Vec<_>>(), [StoryKind::Technical, StoryKind::Story]);
    }

    #[tokio::test]
    async fn stories_with_a_dev_test_or_demo_child_are_flagged() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/wiql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "workItems": [{ "id": 1 }, { "id": 2 }, { "id": 3 }, { "id": 4 }] })))
            .mount(&server)
            .await;
        let link = |id: u32| json!({ "rel": "System.LinkTypes.Hierarchy-Forward", "url": format!("https://dev.azure.com/o/_apis/wit/workItems/{id}") });
        // The links call: 1 has a Demo item, 2 only an unrelated and a removed child, 3 none, 4 a test case.
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .and(body_partial_json(json!({ "$expand": "relations" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": 1, "relations": [link(11)] },
                { "id": 2, "relations": [link(21), link(22)] },
                { "id": 3 },
                { "id": 4, "relations": [link(42), link(41), link(43)] },
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .and(body_partial_json(json!({ "ids": [11, 21, 22, 41, 42, 43] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": 11, "fields": { "System.Title": "Demo - One", "System.State": "New", "Microsoft.VSTS.Scheduling.OriginalEstimate": 0.5 } },
                { "id": 21, "fields": { "System.Title": "Investigate the bug", "System.State": "New" } },
                { "id": 22, "fields": { "System.Title": "Dev - Two", "System.State": "Removed" } },
                { "id": 41, "fields": { "System.Title": "Test - Four", "System.State": "Design", "Microsoft.VSTS.Scheduling.OriginalEstimate": 1.5 } },
                { "id": 42, "fields": { "System.Title": "Dev - Four", "System.State": "Active", "Microsoft.VSTS.Scheduling.OriginalEstimate": 3 } },
                { "id": 43, "fields": { "System.Title": "Demo - Four", "System.State": "New", "Microsoft.VSTS.Scheduling.OriginalEstimate": 0 } },
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .and(body_partial_json(json!({ "ids": [1, 2, 3, 4] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": 1, "fields": { "System.Title": "One", "System.State": "New" } },
                { "id": 2, "fields": { "System.Title": "Two", "System.State": "New" } },
                { "id": 3, "fields": { "System.Title": "Three", "System.State": "New" } },
                { "id": 4, "fields": { "System.Title": "Four", "System.State": "New" } },
            ]})))
            .mount(&server)
            .await;

        let stories = client(&server).stories("it-1", "P\\Sprint 1").await.unwrap();
        let items: Vec<_> = stories.iter().map(|s| s.items.clone()).collect();
        // 1: Demo only; 2: nothing counts; 3: no children; 4: a Test Case and a Dev (its Demo has no hours, so it is not done).
        assert_eq!(items, [vec![TaskKind::Demo], vec![], vec![], vec![TaskKind::Dev, TaskKind::Test]]);
    }

    #[tokio::test]
    async fn stories_follow_the_sprint_board_order_not_the_query_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/wiql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "workItems": [{ "id": 1 }, { "id": 2 }, { "id": 3 }] })))
            .mount(&server)
            .await;
        // The board lists 3 first, mixes in a child task (50) and repeats 1 under another parent.
        Mock::given(method("GET"))
            .and(path("/myorg/My%20Project/My%20Team/_apis/work/teamsettings/iterations/it-1/workitems"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "workItemRelations": [
                { "rel": null, "source": null, "target": { "id": 3 } },
                { "rel": "System.LinkTypes.Hierarchy-Forward", "source": { "id": 3 }, "target": { "id": 50 } },
                { "rel": null, "source": null, "target": { "id": 2 } },
                { "rel": null, "source": null, "target": { "id": 1 } },
                { "rel": "System.LinkTypes.Hierarchy-Forward", "source": { "id": 2 }, "target": { "id": 1 } },
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": 1, "fields": { "System.Title": "One", "System.State": "New" } },
                { "id": 2, "fields": { "System.Title": "Two", "System.State": "New" } },
                { "id": 3, "fields": { "System.Title": "Three", "System.State": "New" } },
            ]})))
            .mount(&server)
            .await;

        let stories = client(&server).stories("it-1", "P\\Sprint 1").await.unwrap();
        assert_eq!(stories.iter().map(|s| s.id).collect::<Vec<_>>(), [3, 2, 1]);
    }

    #[test]
    fn sprint_order_helpers_dedupe_and_keep_unmentioned_ids_last() {
        let response = json!({ "workItemRelations": [
            { "target": { "id": 7 } }, { "target": { "id": 5 } }, { "target": { "id": 7 } }, { "rel": "x" } ] });
        assert_eq!(parse_sprint_order(&response), [7, 5]);
        assert!(parse_sprint_order(&json!({})).is_empty());

        // 9 and 4 are not on the board; 5 and 7 are reordered, the unknown 6 on the board is ignored.
        assert_eq!(merge_order(&[4, 5, 7, 9], &[7, 6, 5]), [7, 5, 4, 9]);
    }

    #[tokio::test]
    async fn stories_still_load_when_the_process_has_no_technical_task_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/wiql"))
            .and(body_partial_json(json!({ "query": payload::wiql_for_iteration("P\\Sprint 1", &["User Story", "Technical Task"]) })))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "message": "TF51005: The query references a work item type that does not exist: Technical Task."
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/wiql"))
            .and(body_partial_json(json!({ "query": payload::wiql_for_iteration("P\\Sprint 1", &["User Story"]) })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "workItems": [{ "id": 3 }] })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": 3, "fields": { "System.Title": "Three", "System.State": "New", "System.WorkItemType": "User Story" } },
            ]})))
            .mount(&server)
            .await;

        let stories = client(&server).stories("it-1", "P\\Sprint 1").await.unwrap();
        assert_eq!(stories.iter().map(|s| s.id).collect::<Vec<_>>(), [3]);
    }

    #[tokio::test]
    async fn sends_pat_as_basic_auth() {
        let server = MockServer::start().await;
        let expected = format!("Basic {}", STANDARD.encode(":secret-pat"));
        Mock::given(method("GET"))
            .and(path("/myorg/_apis/projects/My%20Project/teams/My%20Team/members"))
            .and(header("authorization", expected.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [] })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(&server).members().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_task_posts_json_patch_and_returns_the_new_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitems/$Task"))
            .and(header("content-type", "application/json-patch+json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 501 })))
            .expect(1)
            .mount(&server)
            .await;

        let created = client(&server)
            .create_task("Task", &json!([{ "op": "add", "path": "/fields/System.Title", "value": "x" }]))
            .await
            .unwrap();
        assert_eq!(created.id, 501);
        assert!(created.url.ends_with("/myorg/My%20Project/_workitems/edit/501"));
    }

    #[tokio::test]
    async fn test_cases_are_created_from_the_test_case_work_item_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/myorg/My%20Project/_apis/wit/workitems/$Test%20Case"))
            .and(header("content-type", "application/json-patch+json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 502 })))
            .expect(1)
            .mount(&server)
            .await;

        let created = client(&server).create_task("Test Case", &json!([])).await.unwrap();
        assert_eq!(created.id, 502);
    }

    #[tokio::test]
    async fn move_story_patches_the_sprint_order_with_its_neighbours() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/myorg/My%20Project/My%20Team/_apis/work/iterations/it-1/workitemsorder"))
            .and(query_param("api-version", "7.1"))
            .and(body_partial_json(json!({ "ids": [7], "iterationPath": "P\\Sprint 1", "previousId": 3, "nextId": 0 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 7, "order": 1.5 }])))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).move_story("it-1", "P\\Sprint 1", 7, Some(3), None).await.unwrap();
    }

    #[tokio::test]
    async fn move_story_retries_as_a_preview_when_azure_devops_asks_for_it() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(query_param("api-version", "7.1"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "message": "The requested version \"7.1\" of the resource is under preview. The -preview flag must be supplied."
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(query_param("api-version", "7.1-preview.1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).move_story("it-1", "P\\Sprint 1", 7, None, Some(3)).await.unwrap();
    }

    #[tokio::test]
    async fn comments_use_the_preview_api_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/myorg/My%20Project/_apis/wit/workItems/42/comments"))
            .and(query_param("api-version", "7.1-preview.4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "comments": [
                { "id": 1, "text": "<div>Hello</div>", "createdDate": "2026-10-27T09:00:00Z", "createdBy": { "displayName": "Ann Lee" } }
            ]})))
            .expect(1)
            .mount(&server)
            .await;

        let comments = client(&server).comments(42).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "Ann Lee");
    }

    #[tokio::test]
    async fn update_story_patches_the_work_item_with_json_patch() {
        let server = MockServer::start().await;
        let patch = payload::build_comment_patch("Is this <b>ready</b>?");
        Mock::given(method("PATCH"))
            .and(path("/myorg/My%20Project/_apis/wit/workitems/42"))
            .and(header("content-type", "application/json-patch+json"))
            .and(body_partial_json(patch.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 42 })))
            .expect(1)
            .mount(&server)
            .await;

        client(&server).update_story(42, &patch).await.unwrap();
    }

    #[tokio::test]
    async fn rejected_pat_gives_a_helpful_error_instead_of_following_the_sign_in_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "https://login.example/signin"))
            .mount(&server)
            .await;

        let error = client(&server).iterations().await.unwrap_err();
        assert!(error.contains("did not accept the PAT"), "{error}");
    }

    #[tokio::test]
    async fn api_errors_surface_the_message_from_azure_devops() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "message": "TF401232: Work item does not exist" })))
            .mount(&server)
            .await;

        let error = client(&server).create_task("Task", &json!([])).await.unwrap_err();
        assert!(error.contains("TF401232"), "{error}");
    }

    #[tokio::test]
    async fn fetch_image_sends_the_pat_and_rejects_non_images() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/myorg/p/_apis/wit/attachments/good"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/png").set_body_bytes(vec![1, 2, 3]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/myorg/p/_apis/wit/attachments/page"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/html").set_body_string("<html>"))
            .mount(&server)
            .await;

        let client = client(&server);
        let (mime, bytes) = client.fetch_image(&format!("{}/myorg/p/_apis/wit/attachments/good", server.uri())).await.unwrap();
        assert_eq!((mime.as_str(), bytes), ("image/png", vec![1, 2, 3]));
        assert!(client.fetch_image(&format!("{}/myorg/p/_apis/wit/attachments/page", server.uri())).await.is_err());
    }
}
