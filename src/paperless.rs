use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    blocking::{Client, multipart},
    header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use uuid::Uuid;

pub struct Paperless {
    client: Client,
    base: reqwest::Url,
    tags: Vec<u64>,
}
#[derive(Debug, PartialEq)]
pub enum TaskState {
    Pending,
    Success,
    Failure,
}
#[derive(Deserialize)]
struct Task {
    task_id: Uuid,
    status: String,
}
impl Paperless {
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut token = HeaderValue::from_str(&format!("Token {}", config.api_key))
            .context("Invalid api_key")?;
        token.set_sensitive(true);
        headers.insert(AUTHORIZATION, token);
        // Pin the task response contract across Paperless versions.
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json; version=9"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ngxd/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            base: config.base_url()?,
            tags: config.tag_ids.clone(),
        })
    }
    pub fn upload(&self, snapshot: &Path, filename: &str) -> Result<Uuid> {
        let part = multipart::Part::file(snapshot)?.file_name(filename.to_owned());
        let mut form = multipart::Form::new().part("document", part);
        for id in &self.tags {
            form = form.text("tags", id.to_string());
        }
        let response = self
            .client
            .post(self.base.join("api/documents/post_document/")?)
            .multipart(form)
            .send()?
            .error_for_status()?;
        ensure!(response.status().is_success(), "Upload was not accepted");
        let task: String = response
            .json()
            .context("Upload response must be a JSON task UUID")?;
        Uuid::parse_str(&task).context("Invalid task UUID in upload response")
    }
    pub fn poll(&self, id: Uuid) -> Result<TaskState> {
        let response = self
            .client
            .get(self.base.join("api/tasks/")?)
            .query(&[("task_id", id.to_string())])
            .send()?
            .error_for_status()?;
        ensure!(
            response.status().is_success(),
            "Task query was not accepted"
        );
        let value: serde_json::Value = response.json()?;
        parse_tasks(value, id)
    }
}
fn parse_tasks(value: serde_json::Value, id: Uuid) -> Result<TaskState> {
    let tasks: Vec<Task> = serde_json::from_value(value.get("results").cloned().unwrap_or(value))
        .context("Invalid task list")?;
    let matching: Vec<_> = tasks.iter().filter(|task| task.task_id == id).collect();
    ensure!(matching.len() <= 1, "Ambiguous task status");
    let Some(task) = matching.first() else {
        return Ok(TaskState::Pending);
    };
    match task.status.as_str() {
        "SUCCESS" => Ok(TaskState::Success),
        "FAILURE" | "REVOKED" => Ok(TaskState::Failure),
        "PENDING" | "STARTED" | "RETRY" => Ok(TaskState::Pending),
        other => bail!("Unknown task status {other}; retaining local file"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_matching_confirmed_success_counts() {
        let id = Uuid::new_v4();
        assert_eq!(parse_tasks(json!([]), id).unwrap(), TaskState::Pending);
        assert_eq!(
            parse_tasks(json!([{"task_id":Uuid::new_v4(), "status":"SUCCESS"}]), id).unwrap(),
            TaskState::Pending
        );
        assert_eq!(
            parse_tasks(json!({"results":[{"task_id":id,"status":"SUCCESS"}]}), id).unwrap(),
            TaskState::Success
        );
        assert_eq!(
            parse_tasks(json!([{"task_id":id,"status":"FAILURE"}]), id).unwrap(),
            TaskState::Failure
        );
        assert!(parse_tasks(json!([{"task_id":id,"status":"UNKNOWN"}]), id).is_err());
    }
}
