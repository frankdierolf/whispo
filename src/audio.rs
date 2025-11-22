use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

pub struct AudioRecorder {
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
}

impl AudioRecorder {
    pub fn new() -> Result<Self> {
        Ok(AudioRecorder {
            samples: Arc::new(Mutex::new(Vec::new())),
            sample_rate: 44100, // Default sample rate
            channels: 1, // Default channels
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
            cpal::SampleFormat::F32 => self.build_stream::<f32>(&device, &config.into(), samples)?,
            cpal::SampleFormat::I16 => self.build_stream::<i16>(&device, &config.into(), samples)?,
            cpal::SampleFormat::U16 => self.build_stream::<u16>(&device, &config.into(), samples)?,
            _ => anyhow::bail!("Unsupported sample format"),
        };

        stream.play()?;

        // Keep stream alive by leaking it (we'll stop by dropping the recorder)
        std::mem::forget(stream);

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

    pub fn stop_and_save(&self) -> Result<Vec<u8>> {
        let samples = self.samples.lock().unwrap();

        if samples.is_empty() {
            anyhow::bail!("No audio data recorded");
        }

        // Convert f32 samples to i16 for WAV format
        let i16_samples: Vec<i16> = samples
            .iter()
            .map(|&s| {
                // Clamp to [-1.0, 1.0] range and convert to i16
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * i16::MAX as f32) as i16
            })
            .collect();

        // Write to WAV format in a temporary file
        let temp_dir = std::env::temp_dir();
        let wav_path = temp_dir.join("whispo_temp.wav");
        let mp3_path = temp_dir.join("whispo_temp.mp3");

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
                "-hide_banner",  // Hide FFmpeg banner
                "-loglevel", "error",  // Only show errors
                "-i",
                wav_path.to_str().unwrap(),
                "-codec:a",
                "libmp3lame",
                "-b:a",
                "128k", // 128 kbps bitrate for good quality and compression
                "-y", // Overwrite output file if exists
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
        let mp3_data = std::fs::read(&mp3_path)
            .context("Failed to read converted MP3 file")?;

        // Clean up the temporary MP3 file
        let _ = std::fs::remove_file(&mp3_path);

        // Check if file size exceeds 25 MB (OpenAI's limit)
        const MAX_FILE_SIZE: usize = 25 * 1024 * 1024; // 25 MB
        if mp3_data.len() > MAX_FILE_SIZE {
            let size_mb = mp3_data.len() as f64 / (1024.0 * 1024.0);
            let duration_estimate = samples.len() as f64 / (self.sample_rate as f64 * self.channels as f64);
            anyhow::bail!(
                "Audio file is too large ({size_mb:.2} MB). OpenAI's limit is 25 MB.\n\
                Recording duration: ~{duration_estimate:.1} seconds.\n\
                Try recording a shorter message (recommended: under 45 minutes)."
            );
        }

        Ok(mp3_data)
    }
}
