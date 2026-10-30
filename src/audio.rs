use crate::dsp::{DspControls, DspEngine, dbfs};
use crate::lan::AudioTap;
use cpal::{
    Device, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig,
    SupportedStreamConfigRange,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

#[derive(Clone, Debug, Default)]
pub struct DeviceList {
    pub inputs: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioStatus {
    Idle,
    Running,
    Fault,
}

#[derive(Clone, Debug)]
pub struct TelemetrySnapshot {
    pub input_dbfs: [f32; 2],
    pub output_dbfs: [f32; 2],
    pub a_weighted_dbfs: [f32; 2],
    pub underruns: u64,
    pub overflows: u64,
    pub learning_progress: f32,
    pub error: Option<String>,
}

struct AtomicControls {
    hum_hz: AtomicU32,
    flags: AtomicU32,
    noise: AtomicU32,
    bass: AtomicU32,
    treble: AtomicU32,
    gain: AtomicU32,
    learn_samples: AtomicU64,
}

impl AtomicControls {
    fn new(value: DspControls) -> Self {
        let this = Self {
            hum_hz: AtomicU32::new(0),
            flags: AtomicU32::new(0),
            noise: AtomicU32::new(0),
            bass: AtomicU32::new(0),
            treble: AtomicU32::new(0),
            gain: AtomicU32::new(0),
            learn_samples: AtomicU64::new(0),
        };
        this.store(value);
        this
    }

    fn store(&self, value: DspControls) {
        self.hum_hz
            .store(value.hum_hz.unwrap_or(0.0).to_bits(), Ordering::Relaxed);
        let flags = u32::from(value.low_cut)
            | (u32::from(value.overdrive) << 1)
            | (u32::from(value.muted) << 2)
            | (u32::from(value.cleanup_bypassed) << 3);
        self.flags.store(flags, Ordering::Relaxed);
        self.noise
            .store(value.noise_reduction.to_bits(), Ordering::Relaxed);
        self.bass.store(value.bass_db.to_bits(), Ordering::Relaxed);
        self.treble
            .store(value.treble_db.to_bits(), Ordering::Relaxed);
        self.gain
            .store(value.gain_percent.to_bits(), Ordering::Relaxed);
    }

    fn load(&self) -> DspControls {
        let hum = f32::from_bits(self.hum_hz.load(Ordering::Relaxed));
        let flags = self.flags.load(Ordering::Relaxed);
        DspControls {
            hum_hz: (hum > 0.0).then_some(hum),
            low_cut: flags & 1 != 0,
            overdrive: flags & 2 != 0,
            muted: flags & 4 != 0,
            cleanup_bypassed: flags & 8 != 0,
            noise_reduction: f32::from_bits(self.noise.load(Ordering::Relaxed)),
            bass_db: f32::from_bits(self.bass.load(Ordering::Relaxed)),
            treble_db: f32::from_bits(self.treble.load(Ordering::Relaxed)),
            gain_percent: f32::from_bits(self.gain.load(Ordering::Relaxed)),
        }
    }
}

struct SharedTelemetry {
    input: [AtomicU32; 2],
    output: [AtomicU32; 2],
    a_weighted: [AtomicU32; 2],
    underruns: AtomicU64,
    overflows: AtomicU64,
    learning: AtomicU32,
    error: Mutex<Option<String>>,
    status: AtomicU32,
}

impl Default for SharedTelemetry {
    fn default() -> Self {
        Self {
            input: [
                AtomicU32::new((-60.0_f32).to_bits()),
                AtomicU32::new((-60.0_f32).to_bits()),
            ],
            output: [
                AtomicU32::new((-60.0_f32).to_bits()),
                AtomicU32::new((-60.0_f32).to_bits()),
            ],
            a_weighted: [
                AtomicU32::new((-60.0_f32).to_bits()),
                AtomicU32::new((-60.0_f32).to_bits()),
            ],
            underruns: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            learning: AtomicU32::new(1.0_f32.to_bits()),
            error: Mutex::new(None),
            status: AtomicU32::new(0),
        }
    }
}

pub struct AudioEngine {
    controls: Arc<AtomicControls>,
    telemetry: Arc<SharedTelemetry>,
    input_stream: Option<Stream>,
    output_stream: Option<Stream>,
    lan_tap: Option<AudioTap>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        Self::from_lan_tap(None)
    }

    pub fn with_lan_tap(tap: AudioTap) -> Self {
        Self::from_lan_tap(Some(tap))
    }

    fn from_lan_tap(lan_tap: Option<AudioTap>) -> Self {
        Self {
            controls: Arc::new(AtomicControls::new(DspControls::default())),
            telemetry: Arc::new(SharedTelemetry::default()),
            input_stream: None,
            output_stream: None,
            lan_tap,
        }
    }

    pub fn devices() -> Result<DeviceList, String> {
        let host = cpal::default_host();
        let inputs = host
            .input_devices()
            .map_err(|e| e.to_string())?
            .map(|d| device_label(&d))
            .collect();
        Ok(DeviceList { inputs })
    }

    pub fn set_controls(&self, controls: DspControls) {
        self.controls.store(controls);
    }

    pub fn learn_noise(&self, seconds: f32) {
        let samples = (48_000.0 * seconds.clamp(0.25, 10.0)) as u64;
        self.controls
            .learn_samples
            .store(samples, Ordering::Release);
        self.telemetry
            .learning
            .store(0.0_f32.to_bits(), Ordering::Relaxed);
    }

    pub fn status(&self) -> AudioStatus {
        match self.telemetry.status.load(Ordering::Relaxed) {
            1 => AudioStatus::Running,
            2 => AudioStatus::Fault,
            _ => AudioStatus::Idle,
        }
    }

    pub fn telemetry(&self) -> TelemetrySnapshot {
        TelemetrySnapshot {
            input_dbfs: [0, 1]
                .map(|i| f32::from_bits(self.telemetry.input[i].load(Ordering::Relaxed))),
            output_dbfs: [0, 1]
                .map(|i| f32::from_bits(self.telemetry.output[i].load(Ordering::Relaxed))),
            a_weighted_dbfs: [0, 1]
                .map(|i| f32::from_bits(self.telemetry.a_weighted[i].load(Ordering::Relaxed))),
            underruns: self.telemetry.underruns.load(Ordering::Relaxed),
            overflows: self.telemetry.overflows.load(Ordering::Relaxed),
            learning_progress: f32::from_bits(self.telemetry.learning.load(Ordering::Relaxed)),
            error: self
                .telemetry
                .error
                .lock()
                .ok()
                .and_then(|value| value.clone()),
        }
    }

    pub fn start(&mut self, input_index: usize) -> Result<u32, String> {
        self.stop();
        let host = cpal::default_host();
        let input = host
            .input_devices()
            .map_err(|e| e.to_string())?
            .nth(input_index)
            .ok_or("Selected input is no longer available")?;
        let output = host
            .default_output_device()
            .ok_or("The operating system has no default playback device")?;
        let (input_config, output_config) = select_configs(&input, &output)?;
        let sample_rate = input_config.sample_rate();
        let ring = HeapRb::<[f32; 2]>::new((sample_rate as usize * 120) / 1_000);
        let (mut producer, consumer) = ring.split();
        for _ in 0..(sample_rate as usize * 50) / 1_000 {
            let _ = producer.try_push([0.0; 2]);
        }

        self.telemetry.underruns.store(0, Ordering::Relaxed);
        self.telemetry.overflows.store(0, Ordering::Relaxed);
        if let Ok(mut error) = self.telemetry.error.lock() {
            *error = None;
        }

        let input_stream = match input_config.sample_format() {
            SampleFormat::F32 => build_input::<f32, _>(
                &input,
                input_config.into(),
                producer,
                Arc::clone(&self.controls),
                Arc::clone(&self.telemetry),
                self.lan_tap.clone(),
            )?,
            SampleFormat::I16 => build_input::<i16, _>(
                &input,
                input_config.into(),
                producer,
                Arc::clone(&self.controls),
                Arc::clone(&self.telemetry),
                self.lan_tap.clone(),
            )?,
            SampleFormat::U16 => build_input::<u16, _>(
                &input,
                input_config.into(),
                producer,
                Arc::clone(&self.controls),
                Arc::clone(&self.telemetry),
                self.lan_tap.clone(),
            )?,
            format => return Err(format!("Input sample format {format} is not supported yet")),
        };
        let output_stream = match output_config.sample_format() {
            SampleFormat::F32 => build_output::<f32, _>(
                &output,
                output_config.into(),
                consumer,
                Arc::clone(&self.telemetry),
            )?,
            SampleFormat::I16 => build_output::<i16, _>(
                &output,
                output_config.into(),
                consumer,
                Arc::clone(&self.telemetry),
            )?,
            SampleFormat::U16 => build_output::<u16, _>(
                &output,
                output_config.into(),
                consumer,
                Arc::clone(&self.telemetry),
            )?,
            format => {
                return Err(format!(
                    "Output sample format {format} is not supported yet"
                ));
            }
        };
        output_stream
            .play()
            .map_err(|e| format!("Could not start output: {e}"))?;
        input_stream
            .play()
            .map_err(|e| format!("Could not start input: {e}"))?;
        self.input_stream = Some(input_stream);
        self.output_stream = Some(output_stream);
        self.telemetry.status.store(1, Ordering::Relaxed);
        Ok(sample_rate)
    }

    pub fn stop(&mut self) {
        self.input_stream = None;
        self.output_stream = None;
        self.telemetry.status.store(0, Ordering::Relaxed);
    }
}

fn device_label(device: &Device) -> String {
    device
        .description()
        .map(|d| d.to_string())
        .or_else(|_| device.id().map(|id| id.to_string()))
        .unwrap_or_else(|_| "Unknown device".into())
}

fn select_configs(
    input: &Device,
    output: &Device,
) -> Result<(SupportedStreamConfig, SupportedStreamConfig), String> {
    let inputs: Vec<_> = input
        .supported_input_configs()
        .map_err(|e| format!("Cannot read input formats: {e}"))?
        .filter(supported_format)
        .collect();
    let outputs: Vec<_> = output
        .supported_output_configs()
        .map_err(|e| format!("Cannot read output formats: {e}"))?
        .filter(supported_format)
        .collect();
    let in_rates: Vec<_> = inputs
        .iter()
        .map(|r| (r.min_sample_rate(), r.max_sample_rate()))
        .collect();
    let out_rates: Vec<_> = outputs
        .iter()
        .map(|r| (r.min_sample_rate(), r.max_sample_rate()))
        .collect();
    let rate = choose_shared_rate(&in_rates, &out_rates)
        .ok_or("The selected devices do not share a 48 kHz or 44.1 kHz mode")?;
    let choose = |ranges: Vec<SupportedStreamConfigRange>| {
        ranges
            .into_iter()
            .filter(|r| r.min_sample_rate() <= rate && rate <= r.max_sample_rate())
            .min_by_key(|r| (format_rank(r.sample_format()), r.channels().abs_diff(2)))
            .and_then(|r| r.try_with_sample_rate(rate))
    };
    Ok((
        choose(inputs).ok_or("No compatible input configuration")?,
        choose(outputs).ok_or("No compatible output configuration")?,
    ))
}

fn supported_format(range: &SupportedStreamConfigRange) -> bool {
    matches!(
        range.sample_format(),
        SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16
    ) && range.channels() > 0
}
fn format_rank(format: SampleFormat) -> u8 {
    match format {
        SampleFormat::F32 => 0,
        SampleFormat::I16 => 1,
        SampleFormat::U16 => 2,
        _ => 3,
    }
}

fn choose_shared_rate(input: &[(u32, u32)], output: &[(u32, u32)]) -> Option<u32> {
    [48_000, 44_100].into_iter().find(|rate| {
        input.iter().any(|(min, max)| min <= rate && rate <= max)
            && output.iter().any(|(min, max)| min <= rate && rate <= max)
    })
}

fn input_frame<T: InputSample>(data: &[T], frame: usize, channels: usize) -> [f32; 2] {
    let index = frame * channels;
    let left = data[index].into_f32();
    if channels == 1 {
        [left, left]
    } else {
        [left, data[index + 1].into_f32()]
    }
}

fn i16_to_f32(value: i16) -> f32 {
    if value < 0 {
        value as f32 / 32768.0
    } else {
        value as f32 / 32767.0
    }
}
fn f32_to_i16(value: f32) -> i16 {
    if value <= -1.0 {
        i16::MIN
    } else {
        (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
    }
}
fn u16_to_f32(value: u16) -> f32 {
    (value as f32 - 32768.0) / 32768.0
}
fn f32_to_u16(value: f32) -> u16 {
    (value.clamp(-1.0, 1.0) * 32767.0 + 32768.0).round() as u16
}

trait InputSample: SizedSample + Copy {
    fn into_f32(self) -> f32;
}
impl InputSample for f32 {
    fn into_f32(self) -> f32 {
        self
    }
}
impl InputSample for i16 {
    fn into_f32(self) -> f32 {
        i16_to_f32(self)
    }
}
impl InputSample for u16 {
    fn into_f32(self) -> f32 {
        u16_to_f32(self)
    }
}

trait OutputSample: SizedSample {
    fn from_f32(value: f32) -> Self;
}
impl OutputSample for f32 {
    fn from_f32(value: f32) -> Self {
        value
    }
}
impl OutputSample for i16 {
    fn from_f32(value: f32) -> Self {
        f32_to_i16(value)
    }
}
impl OutputSample for u16 {
    fn from_f32(value: f32) -> Self {
        f32_to_u16(value)
    }
}

fn push_frame<P: Producer<Item = [f32; 2]>>(
    producer: &mut P,
    frame: [f32; 2],
    overflows: &AtomicU64,
) {
    if producer.try_push(frame).is_err() {
        overflows.fetch_add(1, Ordering::Relaxed);
    }
}

fn pop_frame<C: Consumer<Item = [f32; 2]>>(consumer: &mut C, underruns: &AtomicU64) -> [f32; 2] {
    consumer.try_pop().unwrap_or_else(|| {
        underruns.fetch_add(1, Ordering::Relaxed);
        [0.0; 2]
    })
}

fn build_input<T, P>(
    device: &Device,
    config: StreamConfig,
    mut producer: P,
    controls: Arc<AtomicControls>,
    telemetry: Arc<SharedTelemetry>,
    lan_tap: Option<AudioTap>,
) -> Result<Stream, String>
where
    T: InputSample,
    P: Producer<Item = [f32; 2]> + Send + 'static,
{
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate as f32;
    let error_state = Arc::clone(&telemetry);
    let mut dsp = DspEngine::new(sample_rate);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let control = controls.load();
                let learn = controls.learn_samples.swap(0, Ordering::AcqRel);
                if learn > 0 {
                    dsp.begin_noise_learning(learn as usize);
                }
                for frame in data.chunks_exact(channels) {
                    let output = dsp.process_frame(input_frame(frame, 0, channels), &control);
                    push_frame(&mut producer, output, &telemetry.overflows);
                    if let Some(tap) = &lan_tap {
                        let _ = tap.push(output);
                    }
                }
                let levels = dsp.levels();
                for channel in 0..2 {
                    telemetry.input[channel].store(
                        dbfs(levels.input_peak[channel]).to_bits(),
                        Ordering::Relaxed,
                    );
                    telemetry.output[channel].store(
                        dbfs(levels.output_peak[channel]).to_bits(),
                        Ordering::Relaxed,
                    );
                    telemetry.a_weighted[channel]
                        .store(levels.a_weighted_dbfs[channel].to_bits(), Ordering::Relaxed);
                }
                telemetry
                    .learning
                    .store(dsp.noise_learning_progress().to_bits(), Ordering::Relaxed);
            },
            move |error| set_stream_error(&error_state, error.to_string()),
            None,
        )
        .map_err(|e| format!("Could not open input: {e}"))
}

fn build_output<T, C>(
    device: &Device,
    config: StreamConfig,
    mut consumer: C,
    telemetry: Arc<SharedTelemetry>,
) -> Result<Stream, String>
where
    T: OutputSample,
    C: Consumer<Item = [f32; 2]> + Send + 'static,
{
    let channels = config.channels as usize;
    let error_state = Arc::clone(&telemetry);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for frame in data.chunks_exact_mut(channels) {
                    let stereo = pop_frame(&mut consumer, &telemetry.underruns);
                    frame[0] = T::from_f32(stereo[0]);
                    if channels > 1 {
                        frame[1] = T::from_f32(stereo[1]);
                    }
                    for sample in &mut frame[2.min(channels)..] {
                        *sample = T::from_f32(0.0);
                    }
                }
            },
            move |error| set_stream_error(&error_state, error.to_string()),
            None,
        )
        .map_err(|e| format!("Could not open output: {e}"))
}

fn set_stream_error(telemetry: &SharedTelemetry, message: String) {
    telemetry.status.store(2, Ordering::Relaxed);
    if let Ok(mut error) = telemetry.error.lock() {
        *error = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::{HeapRb, traits::Split};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn shared_rate_prefers_48k_then_44k1() {
        assert_eq!(
            choose_shared_rate(&[(44_100, 96_000)], &[(48_000, 48_000)]),
            Some(48_000)
        );
        assert_eq!(
            choose_shared_rate(&[(44_100, 44_100)], &[(44_100, 96_000)]),
            Some(44_100)
        );
        assert_eq!(
            choose_shared_rate(&[(32_000, 32_000)], &[(44_100, 48_000)]),
            None
        );
    }

    #[test]
    fn mono_and_multichannel_input_map_to_stereo() {
        assert_eq!(input_frame(&[0.25, -0.5], 0, 1), [0.25, 0.25]);
        assert_eq!(input_frame(&[0.25, -0.5], 1, 1), [-0.5, -0.5]);
        assert_eq!(input_frame(&[0.1, 0.2, 0.3, 0.4], 1, 2), [0.3, 0.4]);
    }

    #[test]
    fn integer_sample_conversion_uses_full_scale() {
        assert!((i16_to_f32(i16::MAX) - 1.0).abs() < 0.0001);
        assert_eq!(f32_to_i16(-1.0), i16::MIN);
        assert!((u16_to_f32(32_768)).abs() < 0.0001);
        assert_eq!(f32_to_u16(0.0), 32_768);
    }

    #[test]
    fn bounded_transfer_counts_overflow_and_returns_silence_on_underrun() {
        let ring = HeapRb::new(1);
        let (mut producer, mut consumer) = ring.split();
        let overflow = AtomicU64::new(0);
        let underrun = AtomicU64::new(0);
        push_frame(&mut producer, [0.1, 0.2], &overflow);
        push_frame(&mut producer, [0.3, 0.4], &overflow);
        assert_eq!(overflow.load(Ordering::Relaxed), 1);
        assert_eq!(pop_frame(&mut consumer, &underrun), [0.1, 0.2]);
        assert_eq!(pop_frame(&mut consumer, &underrun), [0.0, 0.0]);
        assert_eq!(underrun.load(Ordering::Relaxed), 1);
    }
}
