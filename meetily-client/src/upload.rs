use crate::transcribe::TranscriptSegment;
use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::sync::OnceLock;
use std::time::Duration;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct CreateMeetingResponse {
    id: String,
}

fn http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client")
    })
}

fn api_url(server_url: &str, path: &str) -> String {
    format!("{}/api{}", server_url.trim_end_matches('/'), path)
}

pub async fn create_meeting(server_url: &str, title: &str, client_id: &str) -> Result<String> {
    let client = http_client();
    let url = api_url(server_url, "/meetings");
    let response = client
        .post(&url)
        .json(&json!({
            "title": title,
            "client_id": client_id,
        }))
        .send()
        .await
        .map_err(|e| anyhow!("Failed to connect to server at {}: {}", url, e))?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Server returned {} when creating meeting",
            response.status()
        ));
    }

    let body: CreateMeetingResponse = response.json().await?;
    Ok(body.id)
}

pub async fn upload_transcript(
    server_url: &str,
    meeting_id: &str,
    segments: &[TranscriptSegment],
) -> Result<()> {
    let client = http_client();
    let url = api_url(server_url, &format!("/meetings/{}/transcript", meeting_id));

    let payload: Vec<serde_json::Value> = segments
        .iter()
        .map(|s| {
            json!({
                "timestamp": s.timestamp,
                "text": s.text,
                "source": s.source,
                "confidence": s.confidence,
                "duration_ms": s.duration_ms,
            })
        })
        .collect();

    let response = client
        .post(&url)
        .json(&json!({ "segments": payload }))
        .send()
        .await
        .map_err(|e| anyhow!("Failed to upload transcript: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Server returned {} uploading transcript: {}",
            status,
            body
        ));
    }

    log::info!("Uploaded {} segments to server", segments.len());
    Ok(())
}

pub async fn end_meeting(server_url: &str, meeting_id: &str) -> Result<()> {
    let client = http_client();
    let url = api_url(server_url, &format!("/meetings/{}/end", meeting_id));
    let response = client.post(&url).send().await;

    match response {
        Ok(r) if r.status().is_success() || r.status() == StatusCode::CONFLICT => Ok(()),
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            Err(anyhow!(
                "Server returned {} ending meeting: {}",
                status,
                body
            ))
        }
        Err(e) => Err(anyhow!("Failed to end meeting: {}", e)),
    }
}

pub async fn trigger_summarize(server_url: &str, meeting_id: &str) -> Result<()> {
    let client = http_client();
    let url = api_url(server_url, &format!("/meetings/{}/summarize", meeting_id));
    let response = client.post(&url).send().await?;

    if response.status().is_success() {
        log::info!("Summarization triggered for meeting {}", meeting_id);
        Ok(())
    } else {
        Err(anyhow!(
            "Failed to trigger summarize: {}",
            response.status()
        ))
    }
}
