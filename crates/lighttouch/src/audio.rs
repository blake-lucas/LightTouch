//! Guest audio output for a worker: captured PCM (44.1 kHz stereo S16) goes into a short queue that the host audio
//! device drains, resampled to the device's rate. The queue holds at most ~120 ms so latency stays bounded; when
//! the guest outruns the device the oldest samples go.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const GUEST_RATE: u32 = 44_100;
const MAX_QUEUED_FRAMES: usize = GUEST_RATE as usize * 120 / 1000;

struct Shared {
    queue: Mutex<VecDeque<[i16; 2]>>,
    muted: AtomicBool,
}

pub struct Output {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
}

impl Output {
    pub fn open() -> Result<Self, String> {
        // cpal unwraps some platform errors (Windows without an audio device enumerator): no audio, not no session.
        std::panic::catch_unwind(Self::open_default)
            .unwrap_or_else(|_| Err("the audio system failed to start".into()))
    }

    fn open_default() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let config = device.default_output_config().map_err(|e| format!("audio device configuration: {e}"))?;
        let channels = config.channels() as usize;
        let rate = config.sample_rate();
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::with_capacity(MAX_QUEUED_FRAMES * 2)),
            muted: AtomicBool::new(false),
        });
        let mut resampler = Resampler::new(GUEST_RATE, rate);
        let stream_config: cpal::StreamConfig = config.config();
        let error = |e| eprintln!("audio stream error: {e}");
        macro_rules! build {
            ($t:ty, $convert:expr) => {{
                let shared = shared.clone();
                device.build_output_stream(
                    stream_config.clone(),
                    move |out: &mut [$t], _| {
                        let mut queue = shared.queue.lock().unwrap();
                        let muted = shared.muted.load(Ordering::Relaxed);
                        for frame in out.chunks_mut(channels) {
                            let [l, r] = resampler.next(&mut queue);
                            let (l, r) = if muted { (0.0, 0.0) } else { (l, r) };
                            for (index, sample) in frame.iter_mut().enumerate() {
                                *sample = $convert(if index % 2 == 0 { l } else { r });
                            }
                        }
                    },
                    error,
                    None,
                )
            }};
        }
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build!(f32, |v: f32| v),
            cpal::SampleFormat::I16 => build!(i16, |v: f32| (v * 32767.0) as i16),
            cpal::SampleFormat::U16 => build!(u16, |v: f32| ((v * 32767.0) + 32768.0) as u16),
            cpal::SampleFormat::I32 => build!(i32, |v: f32| (v * 2147483647.0) as i32),
            other => return Err(format!("unsupported audio sample format {other:?}")),
        }
        .map_err(|e| format!("open audio output: {e}"))?;
        stream.play().map_err(|e| format!("start audio output: {e}"))?;
        eprintln!("audio: {} Hz, {channels} channels, {:?}", rate, config.sample_format());
        Ok(Self { shared, _stream: stream })
    }

    /// Queue interleaved stereo samples.
    pub fn push(&self, samples: &[i16]) {
        let mut queue = self.shared.queue.lock().unwrap();
        for pair in samples.chunks_exact(2) {
            queue.push_back([pair[0], pair[1]]);
        }
        let excess = queue.len().saturating_sub(MAX_QUEUED_FRAMES);
        queue.drain(..excess);
    }

    /// Drop queued audio (pause, stop).
    pub fn clear(&self) {
        self.shared.queue.lock().unwrap().clear();
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }
}

/// Linear-interpolating rate converter; plays silence on underrun.
struct Resampler {
    step: f64,
    position: f64,
    previous: [f32; 2],
    current: [f32; 2],
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, position: 1.0, previous: [0.0; 2], current: [0.0; 2] }
    }

    fn next(&mut self, queue: &mut VecDeque<[i16; 2]>) -> [f32; 2] {
        while self.position >= 1.0 {
            self.position -= 1.0;
            self.previous = self.current;
            self.current = match queue.pop_front() {
                Some([l, r]) => [l as f32 / 32768.0, r as f32 / 32768.0],
                None => [0.0; 2],
            };
        }
        let t = self.position as f32;
        self.position += self.step;
        [
            self.previous[0] + (self.current[0] - self.previous[0]) * t,
            self.previous[1] + (self.current[1] - self.previous[1]) * t,
        ]
    }
}
