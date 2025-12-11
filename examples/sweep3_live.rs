use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum WaveformType {
    Sine = 0,
    Saw = 1,
    Square = 2,
    SampleHold = 3, // S/H (random)
}

impl WaveformType {
    fn from_u8(v: u8) -> Self {
        match v % 4 {
            0 => WaveformType::Sine,
            1 => WaveformType::Saw,
            2 => WaveformType::Square,
            3 => WaveformType::SampleHold,
            _ => unreachable!(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            WaveformType::Sine => "Sine",
            WaveformType::Saw => "Saw",
            WaveformType::Square => "Square",
            WaveformType::SampleHold => "S/H",
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum ModulationType {
    Mix = 0,  // Simple addition (mix down)
    Ring = 1, // Ring modulation (multiplication)
    FM = 2,   // Frequency modulation
}

impl ModulationType {
    fn from_u8(v: u8) -> Self {
        match v % 3 {
            0 => ModulationType::Mix,
            1 => ModulationType::Ring,
            2 => ModulationType::FM,
            _ => unreachable!(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            ModulationType::Mix => "MIX (Addition)",
            ModulationType::Ring => "RING (Multiplication)",
            ModulationType::FM => "FM (Frequency Modulation)",
        }
    }
}

struct SweepOscillator {
    phase: f32,
    sample_rate: f32,
    start_freq: f32,
    end_freq: f32,
    duration_samples: f32,
    current_sample: f32,
    reversed: Arc<AtomicBool>,
    waveform: Arc<AtomicU8>,
    sh_value: f32,      // Current S/H held value
    sh_last_phase: f32, // Phase at last S/H trigger
    rng_state: u32,     // Simple LCG state for S/H
}

impl SweepOscillator {
    fn new(
        sample_rate: f32,
        start_freq: f32,
        end_freq: f32,
        duration_secs: f32,
        reversed: Arc<AtomicBool>,
        waveform: Arc<AtomicU8>,
    ) -> Self {
        Self {
            phase: 0.0,
            sample_rate,
            start_freq,
            end_freq,
            duration_samples: duration_secs * sample_rate,
            current_sample: 0.0,
            reversed,
            waveform,
            sh_value: 0.0,
            sh_last_phase: 0.0,
            rng_state: 12345, // Seed
        }
    }

    fn next_random(&mut self) -> f32 {
        // Simple LCG: state = (a * state + c) mod m
        self.rng_state = self.rng_state.wrapping_mul(1103515245).wrapping_add(12345);
        // Convert to [-1, 1] range
        (self.rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn tick(&mut self) -> f32 {
        self.tick_with_fm(0.0)
    }

    fn tick_with_fm(&mut self, freq_mod: f32) -> f32 {
        // Update direction based on toggle state
        if self.reversed.load(Ordering::Relaxed) {
            self.current_sample = (self.current_sample - 1.0).max(0.0);
        } else {
            self.current_sample = (self.current_sample + 1.0).min(self.duration_samples);
        }

        if self.current_sample >= self.duration_samples {
            return 0.0;
        }

        let t = self.current_sample / self.duration_samples;
        let base_freq = self.start_freq * (self.end_freq / self.start_freq).powf(t);

        // Apply frequency modulation (freq_mod is in Hz)
        let freq = (base_freq + freq_mod).max(1.0);

        let waveform = WaveformType::from_u8(self.waveform.load(Ordering::Relaxed));
        let sample = match waveform {
            WaveformType::Sine => (self.phase * std::f32::consts::TAU).sin(),
            WaveformType::Saw => 2.0 * self.phase - 1.0,
            WaveformType::Square => {
                if self.phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            WaveformType::SampleHold => {
                // Trigger new random value on phase wrap
                if self.phase < self.sh_last_phase {
                    self.sh_value = self.next_random();
                }
                self.sh_last_phase = self.phase;
                self.sh_value
            }
        };

        self.phase = (self.phase + freq / self.sample_rate) % 1.0;

        sample
    }

    fn current_freq(&self) -> f32 {
        let t = self.current_sample / self.duration_samples;
        self.start_freq * (self.end_freq / self.start_freq).powf(t)
    }
}

fn main() -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    let config = device.default_output_config()?;

    let reversed = Arc::new(AtomicBool::new(false));
    let reversed_clone = Arc::clone(&reversed);

    let modulation = Arc::new(AtomicU8::new(0)); // Start with Mix mode
    let modulation_clone = Arc::clone(&modulation);

    // Waveform types for each oscillator (0=Sine, 1=Saw, 2=Square, 3=S/H)
    let waveform1 = Arc::new(AtomicU8::new(0));
    let waveform2 = Arc::new(AtomicU8::new(0));
    let waveform3 = Arc::new(AtomicU8::new(0));
    let waveform1_clone = Arc::clone(&waveform1);
    let waveform2_clone = Arc::clone(&waveform2);
    let waveform3_clone = Arc::clone(&waveform3);

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    enable_raw_mode()?;

    println!("Sweep started.\r");
    println!("  Space/Arrow: toggle direction\r");
    println!("  'm': cycle modulation mode\r");
    println!("  '1/2/3': cycle waveform for oscillator 1/2/3\r");
    println!("  'q': quit\r");
    println!("Current mode: {}\r", ModulationType::Mix.name());
    println!(
        "Waveforms: OSC1={}, OSC2={}, OSC3={}\r",
        WaveformType::Sine.name(),
        WaveformType::Sine.name(),
        WaveformType::Sine.name()
    );

    // Keyboard monitoring thread
    std::thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {
            if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                if let Ok(Event::Key(key_event)) = event::read() {
                    // Only react to key press, ignore repeat/release
                    if key_event.kind != KeyEventKind::Press {
                        continue;
                    }

                    match key_event.code {
                        KeyCode::Char('q') => {
                            running_clone.store(false, Ordering::Relaxed);
                            break;
                        }
                        KeyCode::Char('m') => {
                            // Cycle through modulation modes
                            let current = modulation_clone.load(Ordering::Relaxed);
                            let next = (current + 1) % 3;
                            modulation_clone.store(next, Ordering::Relaxed);
                            let mode = ModulationType::from_u8(next);
                            println!("Modulation: {}\r", mode.name());
                        }
                        KeyCode::Char(' ') | KeyCode::Up | KeyCode::Down => {
                            // Toggle direction
                            let current = reversed_clone.load(Ordering::Relaxed);
                            reversed_clone.store(!current, Ordering::Relaxed);
                            let direction = if !current { "↑ UP" } else { "↓ DOWN" };
                            println!("Direction: {}\r", direction);
                        }
                        KeyCode::Char('1') => {
                            let current = waveform1_clone.load(Ordering::Relaxed);
                            let next = (current + 1) % 4;
                            waveform1_clone.store(next, Ordering::Relaxed);
                            let wf = WaveformType::from_u8(next);
                            println!("OSC1 waveform: {}\r", wf.name());
                        }
                        KeyCode::Char('2') => {
                            let current = waveform2_clone.load(Ordering::Relaxed);
                            let next = (current + 1) % 4;
                            waveform2_clone.store(next, Ordering::Relaxed);
                            let wf = WaveformType::from_u8(next);
                            println!("OSC2 waveform: {}\r", wf.name());
                        }
                        KeyCode::Char('3') => {
                            let current = waveform3_clone.load(Ordering::Relaxed);
                            let next = (current + 1) % 4;
                            waveform3_clone.store(next, Ordering::Relaxed);
                            let wf = WaveformType::from_u8(next);
                            println!("OSC3 waveform: {}\r", wf.name());
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    match config.sample_format() {
        cpal::SampleFormat::F32 => run::<f32>(&device, &config.into(), reversed, modulation, waveform1, waveform2, waveform3, &running),
        cpal::SampleFormat::I16 => run::<i16>(&device, &config.into(), reversed, modulation, waveform1, waveform2, waveform3, &running),
        cpal::SampleFormat::I32 => run::<i32>(&device, &config.into(), reversed, modulation, waveform1, waveform2, waveform3, &running),
        fmt => panic!("Unsupported format: {fmt}"),
    }?;

    disable_raw_mode()?;
    println!("\nDone.\r");

    Ok(())
}

fn run<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    reversed: Arc<AtomicBool>,
    modulation: Arc<AtomicU8>,
    waveform1: Arc<AtomicU8>,
    waveform2: Arc<AtomicU8>,
    waveform3: Arc<AtomicU8>,
    running: &Arc<AtomicBool>,
) -> Result<(), anyhow::Error> {
    let sample_rate = config.sample_rate as f32;
    let channels = config.channels as usize;

    // Carrier oscillator (highest frequency)
    let mut osc_a = SweepOscillator::new(sample_rate, 20_000.0, 10.0, 10.0, Arc::clone(&reversed), waveform1);
    // Modulator oscillators
    let mut osc_b = SweepOscillator::new(sample_rate, 10_000.0, 10.0, 10.0, Arc::clone(&reversed), waveform2);
    let mut osc_c = SweepOscillator::new(sample_rate, 5_000.0, 10.0, 10.0, Arc::clone(&reversed), waveform3);

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mod_type = ModulationType::from_u8(modulation.load(Ordering::Relaxed));

            for frame in data.chunks_mut(channels) {
                let output = match mod_type {
                    ModulationType::Mix => {
                        // Simple additive mixing
                        (osc_a.tick() + osc_b.tick() + osc_c.tick()) / 3.0
                    }
                    ModulationType::Ring => {
                        // Ring modulation: multiply all oscillator outputs
                        // This creates sum and difference frequencies
                        osc_a.tick() * osc_b.tick() * osc_c.tick()
                    }
                    ModulationType::FM => {
                        // FM synthesis: osc_c modulates osc_b, which modulates osc_a
                        // Modulation depth scales with modulator frequency
                        let mod_c = osc_c.tick();
                        let mod_depth_b = osc_c.current_freq() * 2.0; // FM index
                        let mod_b = osc_b.tick_with_fm(mod_c * mod_depth_b);
                        let mod_depth_a = osc_b.current_freq() * 2.0;
                        osc_a.tick_with_fm(mod_b * mod_depth_a)
                    }
                };

                let sample = T::from_sample(output * 0.3);
                frame.fill(sample);
            }
        },
        |e| eprintln!("Stream error: {e}"),
        None,
    )?;

    stream.play()?;

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}
