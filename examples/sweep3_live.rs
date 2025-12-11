use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample,
};
use crossterm::{
    event::{self, Event, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct SweepOscillator {
    phase: f32,
    sample_rate: f32,
    start_freq: f32,
    end_freq: f32,
    duration_samples: f32,
    current_sample: f32,
    reversed: Arc<AtomicBool>,
}

impl SweepOscillator {
    fn new(
        sample_rate: f32,
        start_freq: f32,
        end_freq: f32,
        duration_secs: f32,
        reversed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            phase: 0.0,
            sample_rate,
            start_freq,
            end_freq,
            duration_samples: duration_secs * sample_rate,
            current_sample: 0.0,
            reversed,
        }
    }

    fn tick(&mut self) -> f32 {
        // Update direction based on key state
        if self.reversed.load(Ordering::Relaxed) {
            self.current_sample = (self.current_sample - 1.0).max(0.0);
        } else {
            self.current_sample = (self.current_sample + 1.0).min(self.duration_samples);
        }

        // Check if at end (silence)
        if self.current_sample >= self.duration_samples {
            return 0.0;
        }

        let t = self.current_sample / self.duration_samples;
        let freq = self.start_freq * (self.end_freq / self.start_freq).powf(t);

        // Sawtooth wave
        let sample = 2.0 * self.phase - 1.0;

        self.phase = (self.phase + freq / self.sample_rate) % 1.0;

        sample
    }
}

fn main() -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    let config = device.default_output_config()?;

    // Shared state for key press
    let key_pressed = Arc::new(AtomicBool::new(false));
    let key_pressed_clone = Arc::clone(&key_pressed);

    // Enable raw mode for keyboard input
    enable_raw_mode()?;

    println!("Sweep started. Hold any key to reverse. Press 'q' to quit.\r");

    // Spawn keyboard monitoring thread
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    std::thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {
            if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                if let Ok(Event::Key(key_event)) = event::read() {
                    // Quit on 'q'
                    if key_event.code == event::KeyCode::Char('q') {
                        running_clone.store(false, Ordering::Relaxed);
                        break;
                    }

                    match key_event.kind {
                        KeyEventKind::Press => {
                            key_pressed_clone.store(true, Ordering::Relaxed);
                        }
                        KeyEventKind::Release => {
                            key_pressed_clone.store(false, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    match config.sample_format() {
        cpal::SampleFormat::F32 => run::<f32>(&device, &config.into(), key_pressed, &running),
        cpal::SampleFormat::I16 => run::<i16>(&device, &config.into(), key_pressed, &running),
        cpal::SampleFormat::I32 => run::<i32>(&device, &config.into(), key_pressed, &running),
        fmt => panic!("Unsupported format: {fmt}"),
    }?;

    disable_raw_mode()?;
    println!("\nDone.\r");

    Ok(())
}

fn run<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    key_pressed: Arc<AtomicBool>,
    running: &Arc<AtomicBool>,
) -> Result<(), anyhow::Error> {
    let sample_rate = config.sample_rate as f32;
    let channels = config.channels as usize;

    // Sweep A: 20kHz → 10Hz
    let mut osc_a =
        SweepOscillator::new(sample_rate, 20_000.0, 10.0, 10.0, Arc::clone(&key_pressed));

    // Sweep B: 10kHz → 10Hz
    let mut osc_b =
        SweepOscillator::new(sample_rate, 10_000.0, 10.0, 10.0, Arc::clone(&key_pressed));

    // Sweep C: 5kHz → 10Hz
    let mut osc_c =
        SweepOscillator::new(sample_rate, 5_000.0, 10.0, 10.0, Arc::clone(&key_pressed));

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

    // Wait until 'q' is pressed
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}
