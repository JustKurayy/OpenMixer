use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use std::{f32::consts::PI, sync::Arc};

const FFT_SIZE: usize = 512;
const HOP_SIZE: usize = FFT_SIZE / 2;

#[derive(Clone, Copy, Debug)]
pub struct DspControls {
    pub hum_hz: Option<f32>,
    pub low_cut: bool,
    pub noise_reduction: f32,
    pub bass_db: f32,
    pub treble_db: f32,
    pub gain_percent: f32,
    pub overdrive: bool,
    pub muted: bool,
    pub cleanup_bypassed: bool,
}

impl Default for DspControls {
    fn default() -> Self {
        Self {
            hum_hz: Some(50.0),
            low_cut: true,
            noise_reduction: 0.45,
            bass_db: 0.0,
            treble_db: 0.0,
            gain_percent: 25.0,
            overdrive: false,
            muted: false,
            cleanup_bypassed: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Levels {
    pub input_peak: [f32; 2],
    pub input_rms: [f32; 2],
    pub output_peak: [f32; 2],
    pub output_rms: [f32; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeterBand {
    Green,
    Orange,
    Red,
}

pub fn dbfs(amplitude: f32) -> f32 {
    20.0 * amplitude.abs().max(0.001).log10()
}

pub fn meter_band(level_dbfs: f32) -> MeterBand {
    if level_dbfs >= -3.0 {
        MeterBand::Red
    } else if level_dbfs >= -12.0 {
        MeterBand::Orange
    } else {
        MeterBand::Green
    }
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::identity()
    }
}

impl Biquad {
    fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn from_raw(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn high_pass(sample_rate: f32, frequency: f32, q: f32) -> Self {
        let w = 2.0 * PI * frequency / sample_rate;
        let alpha = w.sin() / (2.0 * q);
        let cos = w.cos();
        Self::from_raw(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    fn notch(sample_rate: f32, frequency: f32, q: f32) -> Self {
        let w = 2.0 * PI * frequency / sample_rate;
        let alpha = w.sin() / (2.0 * q);
        Self::from_raw(
            1.0,
            -2.0 * w.cos(),
            1.0,
            1.0 + alpha,
            -2.0 * w.cos(),
            1.0 - alpha,
        )
    }

    fn shelf(sample_rate: f32, frequency: f32, gain_db: f32, high: bool) -> Self {
        if gain_db.abs() < 0.01 {
            return Self::identity();
        }
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w = 2.0 * PI * frequency / sample_rate;
        let cos = w.cos();
        let sin = w.sin();
        let alpha = sin / 2.0 * 2.0_f32.sqrt();
        let beta = 2.0 * a.sqrt() * alpha;
        let (b0, b1, b2, a0, a1, a2) = if high {
            (
                a * ((a + 1.0) + (a - 1.0) * cos + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - beta),
                (a + 1.0) - (a - 1.0) * cos + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - beta,
            )
        } else {
            (
                a * ((a + 1.0) - (a - 1.0) * cos + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - beta),
                (a + 1.0) + (a - 1.0) * cos + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - beta,
            )
        };
        Self::from_raw(b0, b1, b2, a0, a1, a2)
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

struct SpectralReducer {
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    input: [[f32; 2]; FFT_SIZE],
    input_pos: usize,
    since_fft: usize,
    fft: [Vec<Complex32>; 2],
    overlap: [[f32; 2]; FFT_SIZE],
    queue: [[f32; 2]; FFT_SIZE * 2],
    queue_read: usize,
    queue_write: usize,
    queue_len: usize,
    noise: [f32; FFT_SIZE],
    learning_hops: usize,
    learned_hops: usize,
}

impl SpectralReducer {
    fn new() -> Self {
        let mut planner = FftPlanner::new();
        Self {
            forward: planner.plan_fft_forward(FFT_SIZE),
            inverse: planner.plan_fft_inverse(FFT_SIZE),
            input: [[0.0; 2]; FFT_SIZE],
            input_pos: 0,
            since_fft: 0,
            fft: [
                vec![Complex32::default(); FFT_SIZE],
                vec![Complex32::default(); FFT_SIZE],
            ],
            overlap: [[0.0; 2]; FFT_SIZE],
            queue: [[0.0; 2]; FFT_SIZE * 2],
            queue_read: 0,
            queue_write: 0,
            queue_len: 0,
            noise: [0.0; FFT_SIZE],
            learning_hops: 0,
            learned_hops: 0,
        }
    }

    fn begin_learning(&mut self, samples: usize) {
        self.noise.fill(0.0);
        self.learning_hops = (samples / HOP_SIZE).max(1);
        self.learned_hops = 0;
    }

    fn learning_progress(&self) -> f32 {
        if self.learning_hops == 0 {
            1.0
        } else {
            (self.learned_hops as f32 / self.learning_hops as f32).min(1.0)
        }
    }

    fn process(&mut self, frame: [f32; 2], strength: f32) -> [f32; 2] {
        self.input[self.input_pos] = frame;
        self.input_pos = (self.input_pos + 1) % FFT_SIZE;
        self.since_fft += 1;
        if self.since_fft == HOP_SIZE {
            self.since_fft = 0;
            self.run_fft(strength.clamp(0.0, 1.0));
        }
        self.pop().unwrap_or([0.0; 2])
    }

    fn run_fft(&mut self, strength: f32) {
        for channel in 0..2 {
            for n in 0..FFT_SIZE {
                let source = (self.input_pos + n) % FFT_SIZE;
                let window = 0.5 - 0.5 * (2.0 * PI * n as f32 / FFT_SIZE as f32).cos();
                self.fft[channel][n] = Complex32::new(self.input[source][channel] * window, 0.0);
            }
            self.forward.process(&mut self.fft[channel]);
        }

        let learning = self.learned_hops < self.learning_hops;
        for bin in 0..FFT_SIZE {
            let magnitude =
                (self.fft[0][bin].norm_sqr() + self.fft[1][bin].norm_sqr()).sqrt() * 0.5;
            if learning {
                self.noise[bin] += magnitude;
            } else if self.learning_hops > 0 && strength > 0.0 {
                let floor = self.noise[bin] / self.learning_hops as f32;
                let ratio = (magnitude / (floor * 1.7 + 1e-9)).clamp(0.0, 1.0);
                let gain = (1.0 - strength * (1.0 - ratio)).max(0.08);
                self.fft[0][bin] *= gain;
                self.fft[1][bin] *= gain;
            }
        }
        if learning {
            self.learned_hops += 1;
        }

        for channel in 0..2 {
            self.inverse.process(&mut self.fft[channel]);
            for n in 0..FFT_SIZE {
                let window = 0.5 - 0.5 * (2.0 * PI * n as f32 / FFT_SIZE as f32).cos();
                self.overlap[n][channel] +=
                    self.fft[channel][n].re * window * (2.0 / FFT_SIZE as f32);
            }
        }
        for n in 0..HOP_SIZE {
            self.push(self.overlap[n]);
        }
        self.overlap.copy_within(HOP_SIZE..FFT_SIZE, 0);
        self.overlap[HOP_SIZE..].fill([0.0; 2]);
    }

    fn push(&mut self, frame: [f32; 2]) {
        if self.queue_len == self.queue.len() {
            return;
        }
        self.queue[self.queue_write] = frame;
        self.queue_write = (self.queue_write + 1) % self.queue.len();
        self.queue_len += 1;
    }

    fn pop(&mut self) -> Option<[f32; 2]> {
        if self.queue_len == 0 {
            return None;
        }
        let frame = self.queue[self.queue_read];
        self.queue_read = (self.queue_read + 1) % self.queue.len();
        self.queue_len -= 1;
        Some(frame)
    }
}

pub struct DspEngine {
    sample_rate: f32,
    high_pass: [Biquad; 2],
    notches: [[Biquad; 3]; 2],
    bass: [Biquad; 2],
    treble: [Biquad; 2],
    last_hum: Option<f32>,
    last_low_cut: bool,
    last_bass: f32,
    last_treble: f32,
    gain: f32,
    spectral: SpectralReducer,
    levels: Levels,
}

impl DspEngine {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            high_pass: [Biquad::identity(); 2],
            notches: [[Biquad::identity(); 3]; 2],
            bass: [Biquad::identity(); 2],
            treble: [Biquad::identity(); 2],
            last_hum: None,
            last_low_cut: false,
            last_bass: f32::NAN,
            last_treble: f32::NAN,
            gain: 0.0,
            spectral: SpectralReducer::new(),
            levels: Levels::default(),
        }
    }

    pub fn begin_noise_learning(&mut self, samples: usize) {
        self.spectral.begin_learning(samples);
    }
    pub fn noise_learning_progress(&self) -> f32 {
        self.spectral.learning_progress()
    }
    pub fn levels(&self) -> Levels {
        self.levels
    }

    pub fn process_buffer(&mut self, frames: &mut [[f32; 2]], controls: &DspControls) {
        for frame in frames {
            *frame = self.process_frame(*frame, controls);
        }
    }

    pub fn process_frame(&mut self, mut frame: [f32; 2], controls: &DspControls) -> [f32; 2] {
        for sample in &mut frame {
            if !sample.is_finite() {
                *sample = 0.0;
            }
        }
        self.update_levels(frame, true);
        if !controls.cleanup_bypassed {
            self.update_filters(controls);
            for (channel, sample) in frame.iter_mut().enumerate() {
                if controls.low_cut {
                    *sample = self.high_pass[channel].process(*sample);
                }
                if controls.hum_hz.is_some() {
                    for notch in &mut self.notches[channel] {
                        *sample = notch.process(*sample);
                    }
                }
                *sample = self.bass[channel].process(*sample);
                *sample = self.treble[channel].process(*sample);
            }
            frame = self.spectral.process(frame, controls.noise_reduction);
        }
        if controls.muted {
            frame = [0.0; 2];
        } else {
            let cap = if controls.overdrive { 300.0 } else { 100.0 };
            let target = controls.gain_percent.clamp(0.0, cap) / 100.0;
            self.gain += (target - self.gain) * 0.002;
            for sample in &mut frame {
                *sample = (*sample * self.gain).clamp(-1.0, 1.0);
            }
        }
        self.update_levels(frame, false);
        frame
    }

    fn update_filters(&mut self, controls: &DspControls) {
        if self.last_low_cut != controls.low_cut {
            self.high_pass = [Biquad::high_pass(self.sample_rate, 28.0, 0.707); 2];
            self.last_low_cut = controls.low_cut;
        }
        if self.last_hum != controls.hum_hz {
            for channel in 0..2 {
                self.notches[channel] = if let Some(hz) = controls.hum_hz {
                    [
                        Biquad::notch(self.sample_rate, hz, 18.0),
                        Biquad::notch(self.sample_rate, hz * 2.0, 22.0),
                        Biquad::notch(self.sample_rate, hz * 3.0, 24.0),
                    ]
                } else {
                    [Biquad::identity(); 3]
                };
            }
            self.last_hum = controls.hum_hz;
        }
        if (self.last_bass - controls.bass_db).abs() > 0.01 {
            self.bass = [Biquad::shelf(
                self.sample_rate,
                180.0,
                controls.bass_db.clamp(-12.0, 12.0),
                false,
            ); 2];
            self.last_bass = controls.bass_db;
        }
        if (self.last_treble - controls.treble_db).abs() > 0.01 {
            self.treble = [Biquad::shelf(
                self.sample_rate,
                4_500.0,
                controls.treble_db.clamp(-12.0, 12.0),
                true,
            ); 2];
            self.last_treble = controls.treble_db;
        }
    }

    fn update_levels(&mut self, frame: [f32; 2], input: bool) {
        let (peak, rms) = if input {
            (&mut self.levels.input_peak, &mut self.levels.input_rms)
        } else {
            (&mut self.levels.output_peak, &mut self.levels.output_rms)
        };
        for channel in 0..2 {
            peak[channel] = (peak[channel] * 0.995).max(frame[channel].abs());
            rms[channel] = (rms[channel] * 0.98 + frame[channel] * frame[channel] * 0.02).sqrt();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(freq: f32, sample_rate: f32, frames: usize, amplitude: f32) -> Vec<[f32; 2]> {
        (0..frames)
            .map(|n| {
                let value = (TAU * freq * n as f32 / sample_rate).sin() * amplitude;
                [value, value]
            })
            .collect()
    }

    fn rms(frames: &[[f32; 2]]) -> f32 {
        (frames.iter().map(|frame| frame[0] * frame[0]).sum::<f32>() / frames.len() as f32).sqrt()
    }

    #[test]
    fn mute_is_exact_silence_and_gain_obeys_mode_cap() {
        let mut engine = DspEngine::new(48_000.0);
        let mut controls = DspControls {
            cleanup_bypassed: true,
            gain_percent: 300.0,
            overdrive: false,
            ..Default::default()
        };
        for _ in 0..4_000 {
            engine.process_frame([0.2, -0.2], &controls);
        }
        let unity = engine.process_frame([0.2, -0.2], &controls);
        assert!((unity[0] - 0.2).abs() < 0.001 && (unity[1] + 0.2).abs() < 0.001);

        controls.overdrive = true;
        let mut boosted = [0.0; 2];
        for _ in 0..4_000 {
            boosted = engine.process_frame([0.2, -0.2], &controls);
        }
        assert!((boosted[0] - 0.6).abs() < 0.02);

        controls.muted = true;
        assert_eq!(engine.process_frame([0.7, -0.7], &controls), [0.0, 0.0]);
    }

    #[test]
    fn startup_gain_ramps_up_from_silence() {
        let mut engine = DspEngine::new(48_000.0);
        let controls = DspControls {
            cleanup_bypassed: true,
            gain_percent: 25.0,
            ..Default::default()
        };
        let first = engine.process_frame([0.8, -0.8], &controls);
        assert!(first[0].abs() < 0.01 && first[1].abs() < 0.01);
    }

    #[test]
    fn output_is_always_finite_and_bounded() {
        let mut engine = DspEngine::new(48_000.0);
        let controls = DspControls {
            cleanup_bypassed: true,
            overdrive: true,
            gain_percent: 300.0,
            ..Default::default()
        };
        for input in [f32::NAN, f32::INFINITY, -f32::INFINITY, 20.0, -20.0] {
            let output = engine.process_frame([input, input], &controls);
            assert!(
                output
                    .iter()
                    .all(|sample| sample.is_finite() && sample.abs() <= 1.0)
            );
        }
    }

    #[test]
    fn hum_filter_rejects_50_hz_more_than_music() {
        let controls = DspControls {
            hum_hz: Some(50.0),
            noise_reduction: 0.0,
            low_cut: false,
            ..Default::default()
        };
        let mut hum = sine(50.0, 48_000.0, 48_000, 0.5);
        let mut music = sine(1_000.0, 48_000.0, 48_000, 0.5);
        DspEngine::new(48_000.0).process_buffer(&mut hum, &controls);
        DspEngine::new(48_000.0).process_buffer(&mut music, &controls);
        assert!(rms(&hum[24_000..]) < rms(&music[24_000..]) * 0.2);
    }

    #[test]
    fn bypass_at_unity_preserves_normal_samples() {
        let mut engine = DspEngine::new(48_000.0);
        let controls = DspControls {
            cleanup_bypassed: true,
            gain_percent: 100.0,
            ..Default::default()
        };
        for _ in 0..4_000 {
            engine.process_frame([0.0, 0.0], &controls);
        }
        for input in [[0.0, 0.0], [0.25, -0.25], [-0.8, 0.8]] {
            let output = engine.process_frame(input, &controls);
            assert!((output[0] - input[0]).abs() < 0.001);
            assert!((output[1] - input[1]).abs() < 0.001);
        }
    }

    #[test]
    fn learned_steady_noise_is_reduced() {
        let controls = DspControls {
            noise_reduction: 0.8,
            hum_hz: None,
            ..Default::default()
        };
        let noise = sine(3_127.0, 48_000.0, 8_192, 0.08);
        let mut engine = DspEngine::new(48_000.0);
        engine.begin_noise_learning(noise.len());
        let mut learned = noise.clone();
        engine.process_buffer(&mut learned, &controls);
        let mut reduced = noise;
        engine.process_buffer(&mut reduced, &controls);
        assert!(rms(&reduced[2_048..]) < 0.055);
    }

    #[test]
    fn dbfs_and_meter_colors_use_headroom_thresholds() {
        assert_eq!(dbfs(0.0), -60.0);
        assert!((dbfs(0.5) + 6.0206).abs() < 0.001);
        assert_eq!(meter_band(-12.1), MeterBand::Green);
        assert_eq!(meter_band(-12.0), MeterBand::Orange);
        assert_eq!(meter_band(-3.0), MeterBand::Red);
    }
}
