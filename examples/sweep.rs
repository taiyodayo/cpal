use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SizedSample,
};
use std::f32::consts::PI;

const TWO_PI: f32 = 2.0 * PI;

struct SweepOscillator {
    phase: f32,
    sample_rate: f32,
    start_freq: f32,
    end_freq: f32,
    duration_samples: f32,
    current_sample: f32,
}

impl SweepOscillator {
    fn new(sample_rate: f32, start_freq: f32, end_freq: f32, duration_secs: f32) -> Self {
        Self {
            phase: 0.0,
            sample_rate,
            start_freq,
            end_freq,
            duration_samples: duration_secs * sample_rate,
            current_sample: 0.0,
        }
    }

    fn tick(&mut self) -> f32 {
        // Logarithmic sweep (perceptually uniform - equal time per octave)
        let t = (self.current_sample / self.duration_samples).min(1.0);
        let freq = self.start_freq * (self.end_freq / self.start_freq).powf(t);

        let sample = (self.phase * TWO_PI).sin();

        // Phase accumulation
        self.phase = (self.phase + freq / self.sample_rate) % 1.0;
        self.current_sample += 1.0;

        sample
    }

    fn is_finished(&self) -> bool {
        self.current_sample >= self.duration_samples
    }
}

fn main() -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    let config = device.default_output_config()?;

    match config.sample_format() {
        cpal::SampleFormat::F32 => run::<f32>(&device, &config.into()),
        cpal::SampleFormat::I16 => run::<i16>(&device, &config.into()),
        cpal::SampleFormat::I32 => run::<i32>(&device, &config.into()),
        fmt => panic!("Unsupported format: {fmt}"),
    }
}

fn run<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
) -> Result<(), anyhow::Error> {
    let sample_rate = config.sample_rate.0 as f32;
    let channels = config.channels as usize;

    let mut osc = SweepOscillator::new(
        sample_rate,
        10_000.0, // Start: 10 kHz
        10.0,     // End: 10 Hz
        10.0,     // Duration: 10 seconds
    );

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let sample = if osc.is_finished() {
                    T::from_sample(0.0)
                } else {
                    T::from_sample(osc.tick() * 0.3)
                };
                frame.fill(sample);
            }
        },
        |e| eprintln!("Stream error: {e}"),
        None,
    )?;

    stream.play()?;

    // Wait for sweep to complete + small buffer
    std::thread::sleep(std::time::Duration::from_secs(11));

    Ok(())
}
