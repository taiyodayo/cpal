use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use device_query::{DeviceQuery, DeviceState, Keycode};
use std::io::{self, Write};
use std::sync::{Arc, Mutex}; // Import Write for flushing stdout

// ==========================================
// CONFIGURATION
// ==========================================
const NUM_OSCS: usize = 7;
const MASTER_VOLUME: f32 = 0.15;

// ==========================================
// SUPERSAW ENGINE
// ==========================================
pub struct SuperSaw {
    phases: [f32; NUM_OSCS],
    sample_rate: f32,
}

impl SuperSaw {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            phases: [0.0; NUM_OSCS],
            sample_rate,
        }
    }

    // Now accepts `detune` as a parameter
    pub fn tick(&mut self, freq: f32, detune: f32) -> f32 {
        if freq <= 0.0 {
            return 0.0;
        }

        let mut output = 0.0;

        // Spread calculation
        let spread_max = 0.04;
        // Square the detune so 0.0 is pure, and 1.0 is MASSIVE
        let current_spread = detune * detune * spread_max;

        for i in 0..NUM_OSCS {
            let offset_factor = (i as f32) - 3.0;
            let osc_freq = freq * (1.0 + (offset_factor * current_spread));

            // Saw Wave
            let saw_sample = (self.phases[i] * 2.0) - 1.0;
            output += saw_sample;

            // Advance Phase
            self.phases[i] += osc_freq / self.sample_rate;
            if self.phases[i] >= 1.0 {
                self.phases[i] -= 1.0;
            }
        }

        (output / NUM_OSCS as f32) * MASTER_VOLUME
    }
}

// ==========================================
// SHARED STATE
// ==========================================
// We pack both variables into a struct to keep things organized
struct SynthState {
    frequency: f32,
    detune: f32,
}

// ==========================================
// MAIN APPLICATION
// ==========================================
fn main() -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    let config = device.default_output_config()?;

    println!("\n+---------------------------------------------------+");
    println!("|  RUST SUPER-SAW SYNTHESIZER v2                    |");
    println!("+---------------------------------------------------+");
    println!("|  [Z]..[M] -> Play Notes                           |");
    println!("|  [1]      -> Increase Detune (Fatter)             |");
    println!("|  [2]      -> Decrease Detune (Thinner)            |");
    println!("|  [ESC]    -> Quit                                 |");
    println!("+---------------------------------------------------+");

    // Initial State: 0Hz, 0.25 Detune amount
    let state = Arc::new(Mutex::new(SynthState {
        frequency: 0.0,
        detune: 0.25,
    }));

    let state_clone = state.clone();

    // Start Audio Stream
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => run_stream::<f32>(&device, &config.into(), state_clone),
        cpal::SampleFormat::I16 => run_stream::<i16>(&device, &config.into(), state_clone),
        fmt => panic!("Unsupported format: {}", fmt),
    }?;

    stream.play()?;

    let device_state = DeviceState::new();
    let mut last_printed_detune = -1.0; // To prevent spamming the console

    loop {
        let keys: Vec<Keycode> = device_state.get_keys();
        if keys.contains(&Keycode::Escape) {
            break;
        }

        let mut target_freq = 0.0;
        let mut detune_change = 0.0;

        // 1. Check for Detune Keys
        if keys.contains(&Keycode::Key1) {
            detune_change = 0.005;
        }
        if keys.contains(&Keycode::Key2) {
            detune_change = -0.005;
        }

        // 2. Check for Note Keys
        for key in &keys {
            let f = match key {
                Keycode::Z => 261.63,
                Keycode::S => 277.18,
                Keycode::X => 293.66,
                Keycode::D => 311.13,
                Keycode::C => 329.63,
                Keycode::V => 349.23,
                Keycode::G => 369.99,
                Keycode::B => 392.00,
                Keycode::H => 415.30,
                Keycode::N => 440.00,
                Keycode::J => 466.16,
                Keycode::M => 493.88,
                Keycode::Comma => 523.25,
                _ => 0.0,
            };
            if f > 0.0 {
                target_freq = f;
            }
        }

        // 3. Update Shared State
        let mut current_detune_display = 0.0;
        if let Ok(mut lock) = state.lock() {
            lock.frequency = target_freq;

            // Apply detune change and clamp between 0.0 and 1.0
            lock.detune = (lock.detune + detune_change).clamp(0.0, 1.0);
            current_detune_display = lock.detune;
        }

        // 4. Update Display (Only if detune changed significantly)
        if (current_detune_display - last_printed_detune).abs() > 0.001 {
            // \r returns cursor to start of line, allowing us to overwrite it
            print!(
                "\r Detune Amount: {:.1}% | Playing: {:.1} Hz      ",
                current_detune_display * 100.0,
                target_freq
            );
            io::stdout().flush().unwrap();
            last_printed_detune = current_detune_display;
        }

        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    Ok(())
}

fn run_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared_state: Arc<Mutex<SynthState>>,
) -> Result<cpal::Stream, anyhow::Error> {
    let sample_rate = config.sample_rate as f32;
    let channels = config.channels as usize;

    let mut synth = SuperSaw::new(sample_rate);

    // Smoothing variables
    let mut smoothed_freq = 0.0;
    let mut amp_envelope = 0.0;
    // We also smooth detune so it doesn't jump abruptly when you press 1 or 2
    let mut smoothed_detune = 0.25;

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            // Read state once per buffer to save locking overhead
            let (target_freq, target_detune) = {
                let lock = shared_state.lock().unwrap();
                (lock.frequency, lock.detune)
            };

            for frame in data.chunks_mut(channels) {
                // Glide frequency
                if target_freq > 0.0 {
                    smoothed_freq = smoothed_freq * 0.92 + target_freq * 0.08;
                }

                // Glide detune (slower glide for smooth morphing)
                smoothed_detune = smoothed_detune * 0.95 + target_detune * 0.05;

                // Envelope
                let target_amp = if target_freq > 0.0 { 1.0 } else { 0.0 };
                amp_envelope = amp_envelope * 0.9 + target_amp * 0.1;

                // Generate
                let sample = if amp_envelope < 0.001 {
                    0.0
                } else {
                    // Pass the smoothed detune into the synth
                    synth.tick(smoothed_freq, smoothed_detune) * amp_envelope
                };

                for s in frame.iter_mut() {
                    *s = T::from_sample(sample);
                }
            }
        },
        |err| eprintln!("Stream error: {}", err),
        None,
    )?;

    Ok(stream)
}
