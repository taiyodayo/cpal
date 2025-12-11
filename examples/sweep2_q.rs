use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample,
};
use std::f32::consts::PI;

const TWO_PI: f32 = 2.0 * PI;
const DURATION_SECS: f32 = 0.3;

struct SweepOscillator {
    phase: f32,
    sample_rate: f32,
    start_freq: f32,
    end_freq: f32,
    duration_samples: f32,
    current_sample: f32,
    // Logarithmic curve steepness for Q-tip percussion sound
    curve: f32,
}

impl SweepOscillator {
    fn new(sample_rate: f32, start_freq: f32, end_freq: f32, curve: f32) -> Self {
        Self {
            phase: 0.0,
            sample_rate,
            start_freq,
            end_freq,
            duration_samples: DURATION_SECS * sample_rate,
            current_sample: 0.0,
            curve,
        }
    }

    fn tick(&mut self) -> f32 {
        if self.is_finished() {
            return 0.0;
        }

        let t = self.current_sample / self.duration_samples;

        // Logarithmic sweep with curve control
        // curve > 1 = faster initial drop (more percussive)
        // Using exponential curve: freq drops rapidly at start, slowly at end
        let curved_t = 1.0 - (1.0 - t).powf(self.curve);
        let freq = self.start_freq * (self.end_freq / self.start_freq).powf(curved_t);

        // Amplitude envelope: quick attack, exponential decay (percussive)
        let envelope = (-t * 5.0).exp();

        let sample = (self.phase * TWO_PI).sin() * envelope;

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
    let sample_rate = config.sample_rate as f32;
    let channels = config.channels as usize;

    // Q-tip percussion sweeps: high freq → low freq with steep logarithmic curve
    // curve = 3.0 gives a punchy, percussive character
    let mut osc_a = SweepOscillator::new(sample_rate, 8_000.0, 80.0, 3.0);
    let mut osc_b = SweepOscillator::new(sample_rate, 12_000.0, 60.0, 4.0);

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let mixed = (osc_a.tick() + osc_b.tick()) * 0.4;
                let sample = T::from_sample(mixed);
                frame.fill(sample);
            }
        },
        |e| eprintln!("Stream error: {e}"),
        None,
    )?;

    stream.play()?;
    std::thread::sleep(std::time::Duration::from_secs_f32(DURATION_SECS + 0.1));

    Ok(())
}
