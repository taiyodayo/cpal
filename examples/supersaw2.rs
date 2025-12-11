use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use device_query::{DeviceQuery, DeviceState, Keycode};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

// ==========================================
// CONSTANTS & TUNING
// ==========================================
const NUM_OSCS: usize = 7;
const MAX_VOICES: usize = 8;
const MASTER_VOLUME: f32 = 0.4;

// ==========================================
// 1. STEREO SUPER-SAW OSCILLATOR
// ==========================================
#[derive(Clone, Copy)]
pub struct StereoSuperSaw {
    phases: [f32; NUM_OSCS],
    sample_rate: f32,
}

impl StereoSuperSaw {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            phases: [0.0; NUM_OSCS],
            sample_rate,
        }
    }

    pub fn tick(&mut self, freq: f32, detune: f32) -> (f32, f32) {
        if freq <= 0.0 {
            return (0.0, 0.0);
        }

        let mut left_out = 0.0;
        let mut right_out = 0.0;

        let spread_max = 0.04;
        let current_spread = detune * detune * spread_max;

        for i in 0..NUM_OSCS {
            let offset_factor = (i as f32) - 3.0;
            let osc_freq = freq * (1.0 + (offset_factor * current_spread));

            let sample = (self.phases[i] * 2.0) - 1.0;

            // Stereo Panning: Spread 7 oscs from Left to Right
            let pan = (i as f32 / (NUM_OSCS - 1) as f32) * 2.0 - 1.0;
            let l_gain = 0.5 * (1.0 - pan);
            let r_gain = 0.5 * (1.0 + pan);

            left_out += sample * l_gain;
            right_out += sample * r_gain;

            self.phases[i] += osc_freq / self.sample_rate;
            if self.phases[i] >= 1.0 {
                self.phases[i] -= 1.0;
            }
        }

        (left_out / 3.5, right_out / 3.5)
    }
}

// ==========================================
// 2. RESONANT LOW PASS FILTER
// ==========================================
#[derive(Clone, Copy)]
struct LowPassFilter {
    prev_cutoff: f32,
    alpha: f32,
    low: f32,
    band: f32,
    sample_rate: f32,
}

impl LowPassFilter {
    fn new(sample_rate: f32) -> Self {
        Self {
            prev_cutoff: -1.0,
            alpha: 0.0,
            low: 0.0,
            band: 0.0,
            sample_rate,
        }
    }

    fn process(&mut self, input: f32, cutoff_hz: f32, resonance: f32) -> f32 {
        let cutoff = cutoff_hz.clamp(20.0, 18000.0);

        if (cutoff - self.prev_cutoff).abs() > 0.1 {
            self.alpha = 2.0 * std::f32::consts::PI * cutoff / self.sample_rate;
            self.prev_cutoff = cutoff;
        }

        let q = (1.0 - resonance).clamp(0.01, 1.0);

        self.low += self.alpha * self.band;
        let high = input - self.low - (q * self.band);
        self.band += self.alpha * high;

        self.low
    }
}

// ==========================================
// 3. ADSR ENVELOPE
// ==========================================
#[derive(Clone, Copy, PartialEq)]
enum EnvState {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy)]
struct Adsr {
    state: EnvState,
    level: f32,
    sample_rate: f32,
    attack_time: f32,
    decay_time: f32,
    sustain_level: f32,
    release_time: f32,
}

impl Adsr {
    fn new(sample_rate: f32) -> Self {
        Self {
            state: EnvState::Idle,
            level: 0.0,
            sample_rate,
            attack_time: 0.05,
            decay_time: 0.2,
            sustain_level: 0.7,
            release_time: 0.5,
        }
    }

    fn trigger(&mut self) {
        self.state = EnvState::Attack;
    }
    fn release(&mut self) {
        self.state = EnvState::Release;
    }

    fn tick(&mut self) -> f32 {
        let attack_rate = 1.0 / (self.attack_time * self.sample_rate);
        let decay_rate = 1.0 / (self.decay_time * self.sample_rate);
        let release_rate = 1.0 / (self.release_time * self.sample_rate);

        match self.state {
            EnvState::Idle => self.level = 0.0,
            EnvState::Attack => {
                self.level += attack_rate;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = EnvState::Decay;
                }
            }
            EnvState::Decay => {
                self.level -= decay_rate;
                if self.level <= self.sustain_level {
                    self.level = self.sustain_level;
                    self.state = EnvState::Sustain;
                }
            }
            EnvState::Sustain => self.level = self.sustain_level,
            EnvState::Release => {
                self.level -= release_rate;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.state = EnvState::Idle;
                }
            }
        }
        self.level
    }
}

// ==========================================
// 4. SINGLE VOICE
// ==========================================
#[derive(Clone, Copy)]
struct Voice {
    active_note: u32,
    osc: StereoSuperSaw,
    env: Adsr,
    filter_l: LowPassFilter,
    filter_r: LowPassFilter,
}

impl Voice {
    fn new(sample_rate: f32) -> Self {
        Self {
            active_note: 0,
            osc: StereoSuperSaw::new(sample_rate),
            env: Adsr::new(sample_rate),
            filter_l: LowPassFilter::new(sample_rate),
            filter_r: LowPassFilter::new(sample_rate),
        }
    }
}

// ==========================================
// GLOBAL SYNTH STATE
// ==========================================
struct SynthEngine {
    voices: [Voice; MAX_VOICES],
    detune: f32,
    cutoff: f32,
    resonance: f32,
    attack: f32,
}

// ==========================================
// MAIN APP
// ==========================================
fn main() -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    let config = device.default_output_config()?;
    // FIX 1: Use .sample_rate().0 to get the u32 value
    let sample_rate = config.sample_rate() as f32;

    println!("\n+-------------------------------------------------------+");
    println!("|  RUST JP-8080 CLONE (Polyphonic Stereo)               |");
    println!("+-------------------------------------------------------+");
    println!("|  [Z..M] Play Chords                                   |");
    println!("|  [1]/[2] Detune (SuperSaw Width)                      |");
    println!("|  [3]/[4] Filter Cutoff (Brightness)                   |");
    println!("|  [5]/[6] Resonance (Squelch)                          |");
    println!("|  [7]/[8] Attack Speed (Pad vs Pluck)                  |");
    println!("|  [ESC]   Quit                                         |");
    println!("+-------------------------------------------------------+");

    let engine = Arc::new(Mutex::new(SynthEngine {
        voices: [Voice::new(sample_rate); MAX_VOICES],
        detune: 0.35,
        cutoff: 0.5,
        resonance: 0.2,
        attack: 0.01,
    }));

    let engine_clone = engine.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => run_stream::<f32>(&device, &config.into(), engine_clone),
        cpal::SampleFormat::I16 => run_stream::<i16>(&device, &config.into(), engine_clone),
        fmt => panic!("Unsupported format: {}", fmt),
    }?;

    stream.play()?;

    // Keyboard Loop
    let device_state = DeviceState::new();
    let mut last_keys: Vec<u32> = Vec::new();

    loop {
        let keys: Vec<Keycode> = device_state.get_keys();
        if keys.contains(&Keycode::Escape) {
            break;
        }

        let mut current_notes = Vec::new();
        let mut detune_change = 0.0;
        let mut cutoff_change = 0.0;
        let mut res_change = 0.0;
        let mut atk_change = 0.0;

        if keys.contains(&Keycode::Key1) {
            detune_change = -0.01;
        }
        if keys.contains(&Keycode::Key2) {
            detune_change = 0.01;
        }
        if keys.contains(&Keycode::Key3) {
            cutoff_change = -0.01;
        }
        if keys.contains(&Keycode::Key4) {
            cutoff_change = 0.01;
        }
        if keys.contains(&Keycode::Key5) {
            res_change = -0.01;
        }
        if keys.contains(&Keycode::Key6) {
            res_change = 0.01;
        }
        if keys.contains(&Keycode::Key7) {
            atk_change = -0.01;
        }
        if keys.contains(&Keycode::Key8) {
            atk_change = 0.01;
        }

        for key in &keys {
            let note = match key {
                Keycode::Z => 60,
                Keycode::S => 61,
                Keycode::X => 62,
                Keycode::D => 63,
                Keycode::C => 64,
                Keycode::V => 65,
                Keycode::G => 66,
                Keycode::B => 67,
                Keycode::H => 68,
                Keycode::N => 69,
                Keycode::J => 70,
                Keycode::M => 71,
                // FIX 2: SemiColon -> Semicolon
                Keycode::Comma => 72,
                Keycode::L => 73,
                Keycode::Dot => 74,
                Keycode::Semicolon => 75,
                Keycode::Slash => 76,
                _ => 0,
            };
            if note > 0 {
                current_notes.push(note);
            }
        }

        if let Ok(mut eng) = engine.lock() {
            eng.detune = (eng.detune + detune_change).clamp(0.0, 1.0);
            eng.cutoff = (eng.cutoff + cutoff_change).clamp(0.0, 1.0);
            eng.resonance = (eng.resonance + res_change).clamp(0.0, 0.95);
            eng.attack = (eng.attack + atk_change).clamp(0.001, 2.0);

            // Handle Note ONs
            for &note in &current_notes {
                if !last_keys.contains(&note) {
                    let attack_val = eng.attack;
                    if let Some(voice) = eng
                        .voices
                        .iter_mut()
                        .find(|v| v.env.state == EnvState::Idle)
                    {
                        voice.active_note = note;
                        voice.env.attack_time = attack_val;
                        voice.env.trigger();
                    } else {
                        // Stealing
                        eng.voices[0].active_note = note;
                        eng.voices[0].env.attack_time = attack_val;
                        eng.voices[0].env.trigger();
                    }
                }
            }

            // Handle Note OFFs
            for &note in &last_keys {
                if !current_notes.contains(&note) {
                    for voice in eng.voices.iter_mut() {
                        if voice.active_note == note && voice.env.state != EnvState::Idle {
                            voice.env.release();
                        }
                    }
                }
            }

            print!(
                "\r Detune:{:.2} | Cutoff:{:.2} | Res:{:.2} | Atk:{:.2} | Active:{}  ",
                eng.detune,
                eng.cutoff,
                eng.resonance,
                eng.attack,
                current_notes.len()
            );
            io::stdout().flush().unwrap();
        }

        last_keys = current_notes;
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    Ok(())
}

fn run_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    engine: Arc<Mutex<SynthEngine>>,
) -> Result<cpal::Stream, anyhow::Error> {
    let channels = config.channels as usize;

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut eng = engine.lock().unwrap();

            // FIX 3: Copy params to local variables to appease Borrow Checker
            let current_detune = eng.detune;
            let current_cutoff = eng.cutoff;
            let current_resonance = eng.resonance;

            // Pre-calculate filter cutoff Hz from the 0.0-1.0 knob
            let cutoff_hz = 20.0 * (18000.0f32 / 20.0).powf(current_cutoff);

            for frame in data.chunks_mut(channels) {
                let mut mix_l = 0.0;
                let mut mix_r = 0.0;

                for voice in eng.voices.iter_mut() {
                    if voice.env.state == EnvState::Idle {
                        continue;
                    }

                    let env_vol = voice.env.tick();

                    let freq = 440.0 * 2.0f32.powf((voice.active_note as f32 - 69.0) / 12.0);

                    // Uses local variable `current_detune` instead of `eng.detune`
                    let (raw_l, raw_r) = voice.osc.tick(freq, current_detune);

                    let filt_l = voice.filter_l.process(raw_l, cutoff_hz, current_resonance);
                    let filt_r = voice.filter_r.process(raw_r, cutoff_hz, current_resonance);

                    mix_l += filt_l * env_vol;
                    mix_r += filt_r * env_vol;
                }

                mix_l *= MASTER_VOLUME;
                mix_r *= MASTER_VOLUME;

                let mut channel_iter = frame.iter_mut();
                if let Some(sample) = channel_iter.next() {
                    *sample = T::from_sample(mix_l);
                }
                if let Some(sample) = channel_iter.next() {
                    *sample = T::from_sample(mix_r);
                }
            }
        },
        |err| eprintln!("Stream error: {}", err),
        None,
    )?;

    Ok(stream)
}
