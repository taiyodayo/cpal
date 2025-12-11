use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use device_query::{DeviceQuery, DeviceState, Keycode};
use std::sync::{Arc, Mutex};

// ==========================================
// CONFIGURATION
// ==========================================
const NUM_OSCS: usize = 7; // Classic SuperSaw has 7 layers
const DETUNE_AMOUNT: f32 = 0.25; // 0.0 = Thin, 0.5 = Very Wide/Dissonant
const MASTER_VOLUME: f32 = 0.15; // Lower volume to prevent distortion

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

    pub fn tick(&mut self, freq: f32) -> f32 {
        if freq <= 0.0 {
            return 0.0;
        }

        let mut output = 0.0;

        // We use a quadratic curve for spread so small detune amounts
        // give subtle phasing, while large ones give the "swarm" effect.
        let spread_max = 0.03; // Max 3% frequency deviation
        let current_spread = DETUNE_AMOUNT * DETUNE_AMOUNT * spread_max;

        for i in 0..NUM_OSCS {
            // Calculate oscillator offset (-3, -2, -1, 0, 1, 2, 3)
            let offset_factor = (i as f32) - 3.0;

            // Calculate the detuned frequency for this specific layer
            let osc_freq = freq * (1.0 + (offset_factor * current_spread));

            // 1. Generate Naive Sawtooth Wave: Range [-1.0, 1.0]
            // Formula: (phase * 2) - 1
            let saw_sample = (self.phases[i] * 2.0) - 1.0;

            output += saw_sample;

            // 2. Advance Phase
            self.phases[i] += osc_freq / self.sample_rate;
            if self.phases[i] >= 1.0 {
                self.phases[i] -= 1.0;
            }
        }

        // Normalize: Divide by 7 oscillators so we don't clip
        // Then apply master volume
        (output / NUM_OSCS as f32) * MASTER_VOLUME
    }
}

// ==========================================
// MAIN APPLICATION
// ==========================================
fn main() -> Result<(), anyhow::Error> {
    // 1. Setup Audio Host
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("no output device available");
    let config = device.default_output_config()?;

    println!("\n+---------------------------------------------------+");
    println!("|  RUST SUPER-SAW SYNTHESIZER                       |");
    println!("+---------------------------------------------------+");
    println!("|  Controls:                                        |");
    println!("|  [Z] [X] [C] [V] [B] [N] [M]  -> Natural Notes    |");
    println!("|   [S] [D]     [G] [H] [J]     -> Sharp/Flat Keys  |");
    println!("|                                                   |");
    println!("|  Hold keys to play. Press [ESC] to quit.          |");
    println!("+---------------------------------------------------+");

    // 2. Shared State for Frequency Control
    // The main thread writes to this, the audio thread reads from it.
    let current_freq = Arc::new(Mutex::new(0.0f32));
    let freq_clone = current_freq.clone();

    // 3. Start the Audio Stream
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => run_stream::<f32>(&device, &config.into(), freq_clone),
        cpal::SampleFormat::I16 => run_stream::<i16>(&device, &config.into(), freq_clone),
        fmt => panic!("Unsupported format: {}", fmt),
    }?;

    stream.play()?;

    // 4. Keyboard Input Loop (Main Thread)
    let device_state = DeviceState::new();
    loop {
        let keys: Vec<Keycode> = device_state.get_keys();

        if keys.contains(&Keycode::Escape) {
            break;
        }

        // Detect which note to play
        // Priority: If multiple keys are pressed, the last one checked wins
        let mut target_freq = 0.0;

        // Iterate over keys to find a match
        for key in keys {
            let f = match key {
                // Lower Octave
                Keycode::Z => 261.63,     // C4
                Keycode::S => 277.18,     // C#4
                Keycode::X => 293.66,     // D4
                Keycode::D => 311.13,     // D#4
                Keycode::C => 329.63,     // E4
                Keycode::V => 349.23,     // F4
                Keycode::G => 369.99,     // F#4
                Keycode::B => 392.00,     // G4
                Keycode::H => 415.30,     // G#4
                Keycode::N => 440.00,     // A4
                Keycode::J => 466.16,     // A#4
                Keycode::M => 493.88,     // B4
                Keycode::Comma => 523.25, // C5
                _ => 0.0,
            };

            if f > 0.0 {
                target_freq = f;
            }
        }

        // Update the audio thread
        if let Ok(mut lock) = current_freq.lock() {
            *lock = target_freq;
        }

        // Small sleep to reduce CPU usage in the input loop
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    Ok(())
}

fn run_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared_freq: Arc<Mutex<f32>>,
) -> Result<cpal::Stream, anyhow::Error> {
    let sample_rate = config.sample_rate as f32;
    let channels = config.channels as usize;

    let mut synth = SuperSaw::new(sample_rate);

    // Smoothers to prevent clicking
    let mut smoothed_freq = 0.0;
    let mut amp_envelope = 0.0;

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let target_freq = *shared_freq.lock().unwrap();

            for frame in data.chunks_mut(channels) {
                // 1. Smooth Frequency (Glide / Portamento)
                if target_freq > 0.0 {
                    // Slide towards target
                    smoothed_freq = smoothed_freq * 0.92 + target_freq * 0.08;
                }

                // 2. Smooth Amplitude (Attack / Release)
                let target_amp = if target_freq > 0.0 { 1.0 } else { 0.0 };
                // Fast attack, slightly slower release
                amp_envelope = amp_envelope * 0.9 + target_amp * 0.1;

                // 3. Generate Sound
                // If the envelope is basically silent, output 0 to save math
                let sample = if amp_envelope < 0.001 {
                    0.0
                } else {
                    synth.tick(smoothed_freq) * amp_envelope
                };

                // 4. Fill Channels
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
