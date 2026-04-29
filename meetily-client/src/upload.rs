use crate::transcribe::TranscriptSegment;
use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::OnceLock;
use std::time::Duration;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct MeetingIdResponse {
    meeting_id: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Serialize)]
struct SaveTranscriptRequest<'a> {
    meeting_title: &'a str,
    transcripts: &'a [TranscriptSegment],
    folder_path: Option<&'a str>,
}

fn http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client")
    })
}

pub async fn create_meeting(server_url: &str, title: &str, client_id: &str) -> Result<String> {
    let client = http_client();
    let meeting_id = format!("meeting-{client_id}");
    let url = format!("{}/meetings", normalize_server_url(server_url));
    let response = client
        .post(url)
        .json(&json!({
            "id": meeting_id,
            "title": title,
            "client_id": client_id,
        }))
        .send()
        .await;

    match response {
        Ok(response) if response.status().is_success() => {
            let body = response
                .json::<MeetingIdResponse>()
                .await
                .unwrap_or(MeetingIdResponse {
                    meeting_id: None,
                    id: None,
                });
            Ok(body
                .meeting_id
                .or(body.id)
                .unwrap_or_else(|| format!("meeting-{client_id}")))
        }
        Ok(response) if response.status() == StatusCode::NOT_FOUND => Ok(meeting_id),
        Ok(response) if response.status() == StatusCode::METHOD_NOT_ALLOWED => Ok(meeting_id),
        Ok(response) => Err(anyhow!("failed to create meeting: {}", response.status())),
        Err(err) => Err(anyhow!("failed to create meeting: {err}")),
    }
}

pub async fn upload_transcript(
    server_url: &str,
    meeting_id: &str,
    segments: &[TranscriptSegment],
) -> Result<()> {
    let _ = upload_transcript_and_get_meeting_id(server_url, meeting_id, segments).await?;
    Ok(())
}

pub async fn upload_transcript_and_get_meeting_id(
    server_url: &str,
    meeting_id: &str,
    segments: &[TranscriptSegment],
) -> Result<String> {
    let client = http_client();
    let url = format!("{}/save-transcript", normalize_server_url(server_url));
    let response = client
        .post(url)
        .json(&SaveTranscriptRequest {
            meeting_title: meeting_id,
            transcripts: segments,
            folder_path: None,
        })
        .send()
        .await?
        .error_for_status()?;

    let body = response
        .json::<MeetingIdResponse>()
        .await
        .unwrap_or(MeetingIdResponse {
            meeting_id: None,
            id: None,
        });
    Ok(body
        .meeting_id
        .or(body.id)
        .unwrap_or_else(|| meeting_id.to_string()))
}

pub async fn end_meeting(server_url: &str, meeting_id: &str) -> Result<()> {
    let client = http_client();
    let base = normalize_server_url(server_url);
    let response = client
        .post(format!("{base}/end-meeting"))
        .json(&json!({ "meeting_id": meeting_id }))
        .send()
        .await;

    match response {
        Ok(response)
            if response.status().is_success() || response.status() == StatusCode::NOT_FOUND =>
        {
            Ok(())
        }
        Ok(response) => Err(anyhow!("failed to end meeting: {}", response.status())),
        Err(_) => Ok(()),
    }
}

pub async fn trigger_summarize(server_url: &str, meeting_id: &str) -> Result<()> {
    let client = http_client();
    let base = normalize_server_url(server_url);
    let response = client
        .post(format!("{base}/api/meetings/{meeting_id}/summarize"))
        .send()
        .await?;

    if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(anyhow!(
            "failed to trigger summarize: {}",
            response.status()
        ))
    }
}

fn normalize_server_url(server_url: &str) -> String {
    server_url.trim_end_matches('/').to_string()
}
