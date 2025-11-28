use anyhow::{Context, Result};
use reqwest::blocking::multipart;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::audio::AudioChunk;

/// Maximum concurrent API requests to OpenAI
const MAX_CONCURRENT_REQUESTS: usize = 3;
/// Maximum words to search for overlap between chunks
const MAX_OVERLAP_WORDS: usize = 15;
/// API request timeout in seconds
const API_TIMEOUT_SECS: u64 = 300;

#[derive(Deserialize, Debug)]
struct TranscriptionResponse {
    text: String,
}

/// Result of transcribing a single chunk
pub struct ChunkTranscription {
    pub index: usize,
    pub text: String,
    pub has_leading_overlap: bool,
}

/// Transcribe a single audio file (blocking, for simple single-file case)
pub fn transcribe_audio(api_key: &str, audio_data: Vec<u8>) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
        .build()
        .context("Failed to create HTTP client")?;

    let form = multipart::Form::new().text("model", "whisper-1").part(
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
        let error_text = response
            .text()
            .unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("OpenAI API error ({status}): {error_text}");
    }

    let text = response.text().context("Failed to get response text")?;
    let transcription: TranscriptionResponse =
        serde_json::from_str(&text).context("Failed to parse OpenAI API response")?;

    Ok(transcription.text)
}

/// Transcribe a single chunk asynchronously
async fn transcribe_chunk_async(
    client: &reqwest::Client,
    api_key: &str,
    chunk: AudioChunk, // Take ownership to avoid clone
) -> Result<ChunkTranscription> {
    let chunk_index = chunk.index;
    let has_leading_overlap = chunk.has_leading_overlap;

    let form = reqwest::multipart::Form::new()
        .text("model", "whisper-1")
        .part(
            "file",
            reqwest::multipart::Part::bytes(chunk.data) // No clone needed
                .file_name(format!("audio_chunk_{chunk_index}.mp3"))
                .mime_str("audio/mpeg")?,
        );

    let response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send()
        .await
        .context("Failed to send request to OpenAI API")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("OpenAI API error ({status}): {error_text}");
    }

    let text = response
        .text()
        .await
        .context("Failed to get response text")?;
    let transcription: TranscriptionResponse =
        serde_json::from_str(&text).context("Failed to parse OpenAI API response")?;

    Ok(ChunkTranscription {
        index: chunk_index,
        text: transcription.text,
        has_leading_overlap,
    })
}

/// Transcribe multiple chunks in parallel with rate limiting
pub async fn parallel_transcribe(
    api_key: &str,
    chunks: Vec<AudioChunk>,
    progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
) -> Result<String> {
    let total_chunks = chunks.len();

    // Create shared HTTP client with timeout
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
        .build()
        .context("Failed to create HTTP client")?;

    // Semaphore to limit concurrent requests
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let client = Arc::new(client);
    let api_key = Arc::new(api_key.to_string());
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let progress_callback = progress_callback.map(Arc::new);

    // Spawn ALL tasks immediately - they'll wait on semaphore inside
    let mut handles = Vec::with_capacity(total_chunks);

    for chunk in chunks {
        let semaphore = semaphore.clone();
        let client = client.clone();
        let api_key = api_key.clone();
        let completed = completed.clone();
        let progress_callback = progress_callback.clone();

        let handle = tokio::spawn(async move {
            // Acquire permit INSIDE the task - this is the key fix!
            // All tasks spawn immediately, then wait for permits
            let _permit = semaphore.acquire_owned().await?;

            // Transcribe this chunk (no retry - data is consumed by the request)
            let result = transcribe_chunk_async(&client, &api_key, chunk).await;

            let transcription = match result {
                Ok(t) => t,
                Err(e) => return Err(e),
            };

            let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if let Some(ref cb) = progress_callback {
                cb(done, total_chunks);
            }
            Ok(transcription)
        });

        handles.push(handle);
    }

    // Collect results
    let mut results = Vec::with_capacity(total_chunks);
    let mut errors = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok(transcription)) => results.push(transcription),
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(anyhow::anyhow!("Task panicked: {e}")),
        }
    }

    // If any chunks failed, return error with details
    if !errors.is_empty() {
        let error_msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        anyhow::bail!(
            "Failed to transcribe {} of {} chunks:\n{}",
            errors.len(),
            total_chunks,
            error_msgs.join("\n")
        );
    }

    // Sort by index to ensure correct order
    results.sort_by_key(|r| r.index);

    // Merge transcriptions
    Ok(merge_transcriptions(results))
}

/// Merge transcription results, handling overlaps
fn merge_transcriptions(transcriptions: Vec<ChunkTranscription>) -> String {
    if transcriptions.is_empty() {
        return String::new();
    }

    if transcriptions.len() == 1 {
        return transcriptions.into_iter().next().unwrap().text;
    }

    let mut merged = String::new();

    for (i, transcription) in transcriptions.into_iter().enumerate() {
        let text = transcription.text.trim();

        if i == 0 {
            // First chunk - use as-is
            merged.push_str(text);
        } else if transcription.has_leading_overlap {
            // This chunk has overlap - try to find and remove duplicate words
            let cleaned_text = remove_overlap(&merged, text);
            if !merged.ends_with(' ') && !cleaned_text.is_empty() && !cleaned_text.starts_with(' ')
            {
                merged.push(' ');
            }
            merged.push_str(&cleaned_text);
        } else {
            // No overlap - just append with space
            if !merged.ends_with(' ') && !text.is_empty() && !text.starts_with(' ') {
                merged.push(' ');
            }
            merged.push_str(text);
        }
    }

    merged
}

/// Remove overlapping text from the beginning of new_text that matches end of existing_text
fn remove_overlap(existing: &str, new_text: &str) -> String {
    let existing_words: Vec<&str> = existing.split_whitespace().collect();
    let new_words: Vec<&str> = new_text.split_whitespace().collect();

    if existing_words.is_empty() || new_words.is_empty() {
        return new_text.to_string();
    }

    // Look for overlap in the last N words of existing and first N words of new
    // ~2 seconds of audio overlap = roughly 5-15 words
    let search_end = existing_words.len().min(MAX_OVERLAP_WORDS);
    let search_new = new_words.len().min(MAX_OVERLAP_WORDS);

    // Find the longest matching overlap
    let mut best_overlap = 0;

    for overlap_len in 1..=search_end.min(search_new) {
        let end_slice = &existing_words[existing_words.len() - overlap_len..];
        let start_slice = &new_words[..overlap_len];

        // Case-insensitive comparison
        let matches = end_slice
            .iter()
            .zip(start_slice.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b));

        if matches {
            best_overlap = overlap_len;
        }
    }

    if best_overlap > 0 {
        // Skip the overlapping words
        new_words[best_overlap..].join(" ")
    } else {
        new_text.to_string()
    }
}
