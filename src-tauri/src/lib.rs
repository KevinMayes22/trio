mod ado;
mod backend;
mod demo;
mod html;
mod model;
mod payload;
mod service;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use ado::AdoClient;
use backend::Backend;
use demo::Demo;
use model::{Comment, CreateOutcome, CreateRequest, Iteration, Member, StoryDetail, StorySummary, Team};
use settings::Settings;

struct AppState {
    settings_path: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    settings: Settings,
    has_pat: bool,
    backend: Option<Arc<Backend>>,
}

impl Inner {
    fn new(settings: Settings) -> Self {
        // A credential-store failure here just means "no PAT yet"; the user is asked for one.
        let pat = settings::load_pat().ok().flatten();
        let backend = build_backend(&settings, pat.as_deref());
        Self { settings, has_pat: pat.is_some(), backend }
    }

    fn view(&self) -> SettingsView {
        SettingsView {
            org: self.settings.org.clone(),
            project: self.settings.project.clone(),
            team: self.settings.team.clone(),
            demo: self.settings.demo,
            has_pat: self.has_pat,
            configured: self.backend.is_some(),
        }
    }
}

fn build_backend(settings: &Settings, pat: Option<&str>) -> Option<Arc<Backend>> {
    if settings.demo {
        return Some(Arc::new(Backend::Demo(Demo::new())));
    }
    if !settings.has_connection_details() {
        return None;
    }
    let client = AdoClient::production(&settings.org, &settings.project, &settings.team, pat?).ok()?;
    Some(Arc::new(Backend::Ado(client)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    org: String,
    project: String,
    team: String,
    demo: bool,
    has_pat: bool,
    /// True when the app can load data (demo mode, or full details plus a PAT).
    configured: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    org: String,
    project: String,
    demo: bool,
    /// Blank or missing keeps the PAT that is already saved.
    pat: Option<String>,
    #[serde(default)]
    forget_pat: bool,
}

fn backend(state: &State<'_, AppState>) -> Result<Arc<Backend>, String> {
    state
        .inner
        .lock()
        .expect("state lock")
        .backend
        .clone()
        .ok_or_else(|| "Trio is not connected yet. Open Settings to add your Azure DevOps details.".to_string())
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> SettingsView {
    state.inner.lock().expect("state lock").view()
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, input: SettingsInput) -> Result<SettingsView, String> {
    let mut inner = state.inner.lock().expect("state lock");
    let org = settings::normalize_org(&input.org);
    let project = input.project.trim().to_string();
    // The team is chosen from the project's list, not typed here; it only carries over while the
    // organisation and project stay the same.
    let team = if org == inner.settings.org && project == inner.settings.project { inner.settings.team.clone() } else { String::new() };
    let settings = Settings { org, project, team, demo: input.demo };

    if input.forget_pat {
        settings::forget_pat()?;
    }
    if let Some(pat) = input.pat.as_deref().filter(|pat| !pat.trim().is_empty()) {
        settings::store_pat(pat)?;
    }
    settings.save(&state.settings_path)?;

    *inner = Inner::new(settings);
    Ok(inner.view())
}

/// The teams of the project, for the team picker.
#[tauri::command]
async fn list_teams(state: State<'_, AppState>) -> Result<Vec<Team>, String> {
    backend(&state)?.teams().await
}

/// Switches to another team and remembers the choice for next time.
#[tauri::command]
fn set_team(state: State<'_, AppState>, team: String) -> Result<SettingsView, String> {
    let mut inner = state.inner.lock().expect("state lock");
    let mut settings = inner.settings.clone();
    settings.team = team.trim().to_string();
    settings.save(&state.settings_path)?;

    if settings.demo {
        // The demo backend holds the comments posted so far, so it is kept rather than rebuilt.
        inner.settings = settings;
    } else {
        *inner = Inner::new(settings);
    }
    Ok(inner.view())
}

#[tauri::command]
async fn list_iterations(state: State<'_, AppState>) -> Result<Vec<Iteration>, String> {
    backend(&state)?.iterations().await
}

#[tauri::command]
async fn list_stories(
    state: State<'_, AppState>,
    iteration_id: String,
    iteration_path: String,
) -> Result<Vec<StorySummary>, String> {
    backend(&state)?.stories(&iteration_id, &iteration_path).await
}

#[tauri::command]
async fn move_story(
    state: State<'_, AppState>,
    iteration_id: String,
    iteration_path: String,
    id: u32,
    previous_id: Option<u32>,
    next_id: Option<u32>,
) -> Result<(), String> {
    let backend = backend(&state)?;
    service::move_story(&backend, &iteration_id, &iteration_path, id, previous_id, next_id).await
}

#[tauri::command]
async fn get_story(state: State<'_, AppState>, id: u32) -> Result<StoryDetail, String> {
    let backend = backend(&state)?;
    service::story_detail(&backend, id).await
}

#[tauri::command]
async fn list_members(state: State<'_, AppState>) -> Result<Vec<Member>, String> {
    backend(&state)?.members().await
}

#[tauri::command]
async fn create_tasks(state: State<'_, AppState>, request: CreateRequest) -> Result<CreateOutcome, String> {
    let backend = backend(&state)?;
    service::create_tasks(&backend, request).await
}

#[tauri::command]
async fn get_comments(state: State<'_, AppState>, story_id: u32) -> Result<Vec<Comment>, String> {
    let backend = backend(&state)?;
    service::story_comments(&backend, story_id).await
}

#[tauri::command]
async fn add_comment(state: State<'_, AppState>, story_id: u32, text: String) -> Result<(), String> {
    let backend = backend(&state)?;
    service::add_comment(&backend, story_id, &text).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let settings = Settings::load(&settings_path);
            app.manage(AppState { settings_path, inner: Mutex::new(Inner::new(settings)) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_teams,
            set_team,
            list_iterations,
            list_stories,
            move_story,
            get_story,
            list_members,
            create_tasks,
            get_comments,
            add_comment,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
