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
        let err_fn = |err| eprintln!("Error in audio stream: {}", err);

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

        // Write to WAV format in memory
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let spec = hound::WavSpec {
                channels: self.channels,
                sample_rate: self.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };

            let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
            for sample in i16_samples {
                writer.write_sample(sample)?;
            }
            writer.finalize()?;
        }

        Ok(cursor.into_inner())
    }
}
