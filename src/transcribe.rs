use anyhow::{Context, Result};
use reqwest::blocking::multipart;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct TranscriptionResponse {
    text: String,
}

pub fn transcribe_audio(api_key: &str, audio_data: Vec<u8>) -> Result<String> {
    let client = reqwest::blocking::Client::new();

    // Create multipart form
    let form = multipart::Form::new()
        .text("model", "whisper-1")
        .part(
            "file",
            multipart::Part::bytes(audio_data)
                .file_name("audio.mp3")
                .mime_str("audio/mpeg")?,
        );

    let response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send()
        .context("Failed to send request to OpenAI API")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("OpenAI API error ({status}): {error_text}");
    }

    let text = response.text().context("Failed to get response text")?;
    let transcription: TranscriptionResponse = serde_json::from_str(&text)
        .context("Failed to parse OpenAI API response")?;

    Ok(transcription.text)
}
