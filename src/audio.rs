use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

/// Threshold for chunking (files larger than this get split)
const CHUNK_THRESHOLD_BYTES: usize = 20 * 1024 * 1024; // 20 MB
/// Duration of each chunk in seconds
const CHUNK_DURATION_SECS: usize = 300; // 5 minutes
/// Overlap between chunks in seconds (to avoid cutting words)
const CHUNK_OVERLAP_SECS: usize = 2;

/// A chunk of audio data ready for transcription
#[derive(Clone)]
pub struct AudioChunk {
    /// MP3 audio data
    pub data: Vec<u8>,
    /// Chunk index (0-based, for ordering)
    pub index: usize,
    /// Whether this chunk has overlap from the previous chunk
    pub has_leading_overlap: bool,
}

/// Result of stopping a recording - either a single file or multiple chunks
pub enum AudioResult {
    /// Small file that can be transcribed directly
    Single(Vec<u8>),
    /// Large file split into chunks for parallel transcription
    Chunked(Vec<AudioChunk>),
}

pub struct AudioRecorder {
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
    stream: Option<cpal::Stream>,
}

impl AudioRecorder {
    pub fn new() -> Result<Self> {
        Ok(AudioRecorder {
            samples: Arc::new(Mutex::new(Vec::new())),
            sample_rate: 44100, // Default sample rate
            channels: 1,        // Default channels
            stream: None,
        })
    }

    pub fn start_recording(&mut self) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No input device available")?;

        let config = device
            .default_input_config()
            .context("Failed to get default input config")?;

        self.sample_rate = config.sample_rate().0;
        self.channels = config.channels();

        let samples = self.samples.clone();
        samples.lock().unwrap().clear();

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                self.build_stream::<f32>(&device, &config.into(), samples)?
            }
            cpal::SampleFormat::I16 => {
                self.build_stream::<i16>(&device, &config.into(), samples)?
            }
            cpal::SampleFormat::U16 => {
                self.build_stream::<u16>(&device, &config.into(), samples)?
            }
            _ => anyhow::bail!("Unsupported sample format"),
        };

        stream.play()?;

        // Store stream to keep it alive; dropping it will release the microphone
        self.stream = Some(stream);

        Ok(())
    }

    fn build_stream<T>(
        &self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Arc<Mutex<Vec<f32>>>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::Sample + cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let err_fn = |err| eprintln!("Error in audio stream: {err}");

        let stream = device.build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut samples = samples.lock().unwrap();
                for &sample in data {
                    samples.push(cpal::Sample::from_sample(sample));
                }
            },
            err_fn,
            None,
        )?;

        Ok(stream)
    }

    pub fn stop_and_save(&mut self) -> Result<AudioResult> {
        // Drop the stream first to release the microphone
        self.stream = None;

        // Take ownership of samples and clear the buffer to prevent reprocessing
        let samples: Vec<f32> = {
            let mut guard = self.samples.lock().unwrap();
            std::mem::take(&mut *guard)
        };

        if samples.is_empty() {
            anyhow::bail!("No audio data recorded");
        }

        // Try to convert the entire recording first
        let mp3_data = self.samples_to_mp3(&samples, "main")?;

        // If at or under threshold, return as single file (fast path)
        if mp3_data.len() <= CHUNK_THRESHOLD_BYTES {
            return Ok(AudioResult::Single(mp3_data));
        }

        // File is too large - need to chunk it
        let samples_per_second = self.sample_rate as usize * self.channels as usize;
        let chunk_samples = CHUNK_DURATION_SECS * samples_per_second;
        let overlap_samples = CHUNK_OVERLAP_SECS * samples_per_second;

        let mut chunks = Vec::new();
        let mut chunk_start = 0usize;
        let mut chunk_index = 0usize;

        while chunk_start < samples.len() {
            let chunk_end = (chunk_start + chunk_samples).min(samples.len());
            let chunk_slice = &samples[chunk_start..chunk_end];

            // Convert this chunk to MP3
            let chunk_mp3 = self.samples_to_mp3(chunk_slice, &format!("chunk{chunk_index}"))?;

            chunks.push(AudioChunk {
                data: chunk_mp3,
                index: chunk_index,
                has_leading_overlap: chunk_index > 0,
            });

            chunk_index += 1;

            // Check if we've reached the end
            if chunk_end >= samples.len() {
                break;
            }

            // Move to next chunk, stepping back by overlap amount
            chunk_start = chunk_end.saturating_sub(overlap_samples);
        }

        Ok(AudioResult::Chunked(chunks))
    }

    /// Convert raw f32 samples to MP3 data
    fn samples_to_mp3(&self, samples: &[f32], suffix: &str) -> Result<Vec<u8>> {
        // Convert f32 samples to i16 for WAV format
        let i16_samples: Vec<i16> = samples
            .iter()
            .map(|&s| {
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * i16::MAX as f32) as i16
            })
            .collect();

        // Use unique temp file names to support parallel FFmpeg calls
        let temp_dir = std::env::temp_dir();
        let unique_id = format!(
            "{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            suffix
        );
        let wav_path = temp_dir.join(format!("whis_{unique_id}.wav"));
        let mp3_path = temp_dir.join(format!("whis_{unique_id}.mp3"));

        {
            let spec = hound::WavSpec {
                channels: self.channels,
                sample_rate: self.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };

            let mut writer = hound::WavWriter::create(&wav_path, spec)?;
            for sample in i16_samples {
                writer.write_sample(sample)?;
            }
            writer.finalize()?;
        }

        // Convert WAV to MP3 using FFmpeg
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                wav_path.to_str().unwrap(),
                "-codec:a",
                "libmp3lame",
                "-b:a",
                "128k",
                "-y",
                mp3_path.to_str().unwrap(),
            ])
            .output()
            .context("Failed to execute ffmpeg. Make sure ffmpeg is installed.")?;

        // Clean up the temporary WAV file
        let _ = std::fs::remove_file(&wav_path);

        if !output.status.success() {
            let _ = std::fs::remove_file(&mp3_path);
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("FFmpeg conversion failed: {stderr}");
        }

        // Read the MP3 file
        let mp3_data = std::fs::read(&mp3_path).context("Failed to read converted MP3 file")?;

        // Clean up the temporary MP3 file
        let _ = std::fs::remove_file(&mp3_path);

        Ok(mp3_data)
    }
}
