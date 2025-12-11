use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample,
};

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
        if self.is_finished() {
            return 0.0;
        }

        let t = self.current_sample / self.duration_samples;
        let freq = self.start_freq * (self.end_freq / self.start_freq).powf(t);

        // Sawtooth wave: ramp from -1 to +1 over cycle
        let sample = 2.0 * self.phase - 1.0;

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

    // Sweep A: 20kHz → 10Hz
    let mut osc_a = SweepOscillator::new(sample_rate, 20_000.0, 10.0, 10.0);

    // Sweep B: 10kHz → 10Hz
    let mut osc_b = SweepOscillator::new(sample_rate, 10_000.0, 10.0, 10.0);

    // Sweep C: 5kHz → 10Hz
    let mut osc_c = SweepOscillator::new(sample_rate, 5_000.0, 10.0, 10.0);

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let mixed = (osc_a.tick() + osc_b.tick() + osc_c.tick()) / 3.0 * 0.3;
                let sample = T::from_sample(mixed);
                frame.fill(sample);
            }
        },
        |e| eprintln!("Stream error: {e}"),
        None,
    )?;

    stream.play()?;
    std::thread::sleep(std::time::Duration::from_secs(11));

    Ok(())
}
