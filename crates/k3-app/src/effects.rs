use std::time::Duration;

use k3_core::VocalEffectPreset;

const COMB_DELAYS_MS: [f32; 4] = [29.7, 37.1, 41.1, 43.7];

pub struct VocalEffect {
    engine: Option<StereoReverb>,
    tail_frames: usize,
}

impl VocalEffect {
    pub fn new(preset: VocalEffectPreset, sample_rate: u32) -> Self {
        let settings = EffectSettings::for_preset(preset);
        Self {
            engine: settings.map(|settings| StereoReverb::new(settings, sample_rate)),
            tail_frames: settings.map_or(0, |settings| {
                milliseconds_to_frames(settings.tail_ms, sample_rate)
            }),
        }
    }

    pub fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        self.engine
            .as_mut()
            .map_or((left, right), |engine| engine.process(left, right))
    }

    pub const fn tail_frames(&self) -> usize {
        self.tail_frames
    }
}

#[derive(Clone, Copy)]
struct EffectSettings {
    dry: f32,
    wet: f32,
    feedback: f32,
    damping: f32,
    pre_delay_ms: f32,
    room_scale: f32,
    echo_ms: f32,
    echo_gain: f32,
    tail_ms: f32,
}

impl EffectSettings {
    fn for_preset(preset: VocalEffectPreset) -> Option<Self> {
        match preset {
            VocalEffectPreset::Clean => None,
            VocalEffectPreset::Studio => Some(Self {
                dry: 0.92,
                wet: 0.16,
                feedback: 0.48,
                damping: 0.35,
                pre_delay_ms: 8.0,
                room_scale: 0.72,
                echo_ms: 70.0,
                echo_gain: 0.04,
                tail_ms: 300.0,
            }),
            VocalEffectPreset::Ktv => Some(Self {
                dry: 0.88,
                wet: 0.28,
                feedback: 0.57,
                damping: 0.40,
                pre_delay_ms: 18.0,
                room_scale: 0.90,
                echo_ms: 110.0,
                echo_gain: 0.18,
                tail_ms: 650.0,
            }),
            VocalEffectPreset::Theater => Some(Self {
                dry: 0.82,
                wet: 0.38,
                feedback: 0.68,
                damping: 0.50,
                pre_delay_ms: 28.0,
                room_scale: 1.25,
                echo_ms: 160.0,
                echo_gain: 0.10,
                tail_ms: 1_100.0,
            }),
            VocalEffectPreset::Church => Some(Self {
                dry: 0.75,
                wet: 0.52,
                feedback: 0.82,
                damping: 0.58,
                pre_delay_ms: 45.0,
                room_scale: 1.55,
                echo_ms: 240.0,
                echo_gain: 0.08,
                tail_ms: 2_000.0,
            }),
        }
    }
}

struct StereoReverb {
    left: ReverbChannel,
    right: ReverbChannel,
}

impl StereoReverb {
    fn new(settings: EffectSettings, sample_rate: u32) -> Self {
        Self {
            left: ReverbChannel::new(settings, sample_rate, 0.0),
            right: ReverbChannel::new(settings, sample_rate, 1.3),
        }
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        (self.left.process(left), self.right.process(right))
    }
}

struct ReverbChannel {
    settings: EffectSettings,
    pre_delay: DelayLine,
    combs: Vec<CombFilter>,
    diffusers: [AllPassFilter; 2],
    echo: FeedbackDelay,
}

impl ReverbChannel {
    fn new(settings: EffectSettings, sample_rate: u32, stereo_spread_ms: f32) -> Self {
        let combs = COMB_DELAYS_MS
            .into_iter()
            .map(|delay_ms| {
                CombFilter::new(
                    milliseconds_to_frames(
                        delay_ms * settings.room_scale + stereo_spread_ms,
                        sample_rate,
                    ),
                    settings.feedback,
                    settings.damping,
                )
            })
            .collect();
        Self {
            settings,
            pre_delay: DelayLine::new(milliseconds_to_frames(
                settings.pre_delay_ms + stereo_spread_ms,
                sample_rate,
            )),
            combs,
            diffusers: [
                AllPassFilter::new(milliseconds_to_frames(5.0 + stereo_spread_ms, sample_rate)),
                AllPassFilter::new(milliseconds_to_frames(1.7 + stereo_spread_ms, sample_rate)),
            ],
            echo: FeedbackDelay::new(
                milliseconds_to_frames(settings.echo_ms + stereo_spread_ms, sample_rate),
                0.28,
            ),
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let room_input = self.pre_delay.process(input);
        let mut wet = self
            .combs
            .iter_mut()
            .map(|comb| comb.process(room_input))
            .sum::<f32>()
            / 4.0;
        for diffuser in &mut self.diffusers {
            wet = diffuser.process(wet);
        }
        let echo = self.echo.process(input);
        input * self.settings.dry + wet * self.settings.wet + echo * self.settings.echo_gain
    }
}

struct DelayLine {
    samples: Vec<f32>,
    cursor: usize,
}

impl DelayLine {
    fn new(frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames.max(1)],
            cursor: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.samples[self.cursor];
        self.samples[self.cursor] = input;
        self.cursor = (self.cursor + 1) % self.samples.len();
        output
    }
}

struct CombFilter {
    delay: DelayLine,
    feedback: f32,
    damping: f32,
    filtered: f32,
}

impl CombFilter {
    fn new(frames: usize, feedback: f32, damping: f32) -> Self {
        Self {
            delay: DelayLine::new(frames),
            feedback,
            damping,
            filtered: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.delay.samples[self.delay.cursor];
        self.filtered = delayed * (1.0 - self.damping) + self.filtered * self.damping;
        self.delay.process(input + self.filtered * self.feedback);
        delayed
    }
}

struct AllPassFilter {
    delay: DelayLine,
}

impl AllPassFilter {
    fn new(frames: usize) -> Self {
        Self {
            delay: DelayLine::new(frames),
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.delay.samples[self.delay.cursor];
        let output = delayed - input;
        self.delay.process(input + delayed * 0.5);
        output
    }
}

struct FeedbackDelay {
    delay: DelayLine,
    feedback: f32,
}

impl FeedbackDelay {
    fn new(frames: usize, feedback: f32) -> Self {
        Self {
            delay: DelayLine::new(frames),
            feedback,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.delay.samples[self.delay.cursor];
        self.delay.process(input + delayed * self.feedback);
        delayed
    }
}

fn milliseconds_to_frames(milliseconds: f32, sample_rate: u32) -> usize {
    let duration = Duration::from_secs_f32(milliseconds / 1_000.0);
    let frames = duration.as_nanos().saturating_mul(u128::from(sample_rate)) / 1_000_000_000;
    usize::try_from(frames).unwrap_or(usize::MAX).max(1)
}
