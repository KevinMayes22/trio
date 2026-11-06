use serde_json::Value;

use crate::ado::AdoClient;
use crate::demo::Demo;
use crate::model::{CreatedTask, Iteration, Member, RawChild, RawComment, RawStory, StorySummary, Team};

/// Either real Azure DevOps or the offline demo. An enum keeps the async calls simple
/// (async trait methods cannot be used as trait objects).
pub enum Backend {
    Ado(AdoClient),
    Demo(Demo),
}

impl Backend {
    pub async fn teams(&self) -> Result<Vec<Team>, String> {
        match self {
            Backend::Ado(ado) => ado.teams().await,
            Backend::Demo(demo) => Ok(demo.teams()),
        }
    }

    pub async fn iterations(&self) -> Result<Vec<Iteration>, String> {
        match self {
            Backend::Ado(ado) => ado.iterations().await,
            Backend::Demo(demo) => Ok(demo.iterations()),
        }
    }

    pub async fn stories(&self, iteration_id: &str, iteration_path: &str) -> Result<Vec<StorySummary>, String> {
        match self {
            Backend::Ado(ado) => ado.stories(iteration_id, iteration_path).await,
            Backend::Demo(demo) => Ok(demo.stories(iteration_path)),
        }
    }

    pub async fn story(&self, id: u32) -> Result<RawStory, String> {
        match self {
            Backend::Ado(ado) => ado.story(id).await,
            Backend::Demo(demo) => demo.story(id),
        }
    }

    pub async fn children(&self, ids: &[u32]) -> Result<Vec<RawChild>, String> {
        match self {
            Backend::Ado(ado) => ado.children(ids).await,
            Backend::Demo(demo) => Ok(demo.children(ids)),
        }
    }

    pub async fn comments(&self, id: u32) -> Result<Vec<RawComment>, String> {
        match self {
            Backend::Ado(ado) => ado.comments(id).await,
            Backend::Demo(demo) => Ok(demo.comments(id)),
        }
    }

    pub async fn members(&self) -> Result<Vec<Member>, String> {
        match self {
            Backend::Ado(ado) => ado.members().await,
            Backend::Demo(demo) => Ok(demo.members()),
        }
    }

    pub async fn create_task(&self, work_item_type: &str, patch: &Value) -> Result<CreatedTask, String> {
        match self {
            Backend::Ado(ado) => ado.create_task(work_item_type, patch).await,
            Backend::Demo(demo) => Ok(demo.create_task(work_item_type, patch)),
        }
    }

    pub async fn update_story(&self, id: u32, patch: &Value) -> Result<(), String> {
        match self {
            Backend::Ado(ado) => ado.update_story(id, patch).await,
            Backend::Demo(demo) => {
                demo.update_story(id, patch);
                Ok(())
            }
        }
    }

    pub async fn move_story(
        &self,
        iteration_id: &str,
        iteration_path: &str,
        id: u32,
        previous_id: Option<u32>,
        next_id: Option<u32>,
    ) -> Result<(), String> {
        match self {
            Backend::Ado(ado) => ado.move_story(iteration_id, iteration_path, id, previous_id, next_id).await,
            Backend::Demo(demo) => demo.move_story(iteration_path, id, previous_id, next_id),
        }
    }

    pub fn web_url(&self, id: u32) -> String {
        match self {
            Backend::Ado(ado) => ado.web_url(id),
            Backend::Demo(demo) => demo.web_url(id),
        }
    }

    pub fn parent_url(&self, id: u32) -> String {
        match self {
            Backend::Ado(ado) => ado.parent_url(id),
            Backend::Demo(demo) => demo.parent_url(id),
        }
    }

    pub fn can_fetch_image(&self, url: &str) -> bool {
        match self {
            Backend::Ado(ado) => ado.can_fetch_image(url),
            Backend::Demo(demo) => demo.can_fetch_image(url),
        }
    }

    pub async fn fetch_image(&self, url: &str) -> Result<(String, Vec<u8>), String> {
        match self {
            Backend::Ado(ado) => ado.fetch_image(url).await,
            Backend::Demo(demo) => demo.fetch_image(url),
        }
    }
}
