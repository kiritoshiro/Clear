//! Voice / ambience / voice-reverb separation. Constants come from MECHANICS.md.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub const KNOB_MIN_DB: f32 = -60.0;
pub const KNOB_MAX_DB: f32 = 12.0;
pub const DEFAULT_AMBIENCE_DB: f32 = -60.0;
pub const DEFAULT_VOICE_DB: f32 = 0.0;
pub const DEFAULT_REVERB_DB: f32 = -60.0;

const BASE_FRAME: usize = 2048;
const POWER_SMOOTH: f32 = 0.7;
const NOISE_WINDOW_S: f32 = 1.5;
const NOISE_SUBWINDOWS: usize = 8;
const NOISE_BIAS: f32 = 1.6;
const NOISE_WARMUP_S: f32 = 3.0;
const REVERB_T60_S: f32 = 0.5;
const REVERB_DELAY_S: f32 = 0.048;
const DD_ALPHA: f32 = 0.92;
const RUMBLE_HZ: f32 = 80.0;

pub fn db_to_gain(db: f32) -> f32 {
    if db <= KNOB_MIN_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

#[derive(Clone)]
pub struct Audio {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl Audio {
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, |c| c.len())
    }
    pub fn seconds(&self) -> f32 {
        self.len() as f32 / self.rate.max(1) as f32
    }
}

pub struct Stems {
    pub voice: Vec<Vec<f32>>,
    pub ambience: Vec<Vec<f32>>,
    pub reverb: Vec<Vec<f32>>,
}

impl Stems {
    /// Mix the three parts with linear gains.
    pub fn mix(&self, gv: f32, ga: f32, gr: f32) -> Vec<Vec<f32>> {
        (0..self.voice.len())
            .map(|c| {
                let (v, a, r) = (&self.voice[c], &self.ambience[c], &self.reverb[c]);
                (0..v.len())
                    .map(|i| gv * v[i] + ga * a[i] + gr * r[i])
                    .collect()
            })
            .collect()
    }
}

pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(1),
            cancel: AtomicBool::new(false),
        })
    }
    pub fn fraction(&self) -> f32 {
        self.done.load(Ordering::Relaxed) as f32 / self.total.load(Ordering::Relaxed).max(1) as f32
    }
}

fn frame_size(rate: u32) -> usize {
    let ratio = (rate as f32 / 48_000.0).max(0.01);
    let pow = ratio.log2().round() as i32;
    let n = if pow >= 0 {
        BASE_FRAME << pow
    } else {
        BASE_FRAME >> (-pow)
    };
    n.max(1024)
}

/// Split every channel into voice, ambience and reverb, in parallel threads.
pub fn separate(audio: &Audio, progress: &Arc<Progress>) -> Option<Stems> {
    let n = frame_size(audio.rate);
    let hop = n / 4;
    let frames_per_ch = audio.len().div_ceil(hop) + 4;
    progress
        .total
        .store(frames_per_ch * audio.channels.len(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);

    let results: Vec<Option<[Vec<f32>; 3]>> = std::thread::scope(|s| {
        let handles: Vec<_> = audio
            .channels
            .iter()
            .map(|ch| {
                let p = progress.clone();
                let rate = audio.rate;
                s.spawn(move || separate_channel(ch, rate, n, &p))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().flatten())
            .collect()
    });

    let mut stems = Stems {
        voice: vec![],
        ambience: vec![],
        reverb: vec![],
    };
    for r in results {
        let [v, a, rv] = r?;
        stems.voice.push(v);
        stems.ambience.push(a);
        stems.reverb.push(rv);
    }
    Some(stems)
}

struct NoiseTracker {
    sub_len: usize,
    count: usize,
    cur_min: Vec<f32>,
    mins: Vec<Vec<f32>>,
    slot: usize,
}

impl NoiseTracker {
    fn new(bins: usize, fps: f32) -> Self {
        let sub_len = ((NOISE_WINDOW_S * fps) / NOISE_SUBWINDOWS as f32)
            .round()
            .max(1.0) as usize;
        Self {
            sub_len,
            count: 0,
            cur_min: vec![f32::MAX; bins],
            mins: vec![vec![f32::MAX; bins]; NOISE_SUBWINDOWS],
            slot: 0,
        }
    }

    fn update(&mut self, ps: &[f32], noise: &mut [f32]) {
        for (m, &p) in self.cur_min.iter_mut().zip(ps) {
            *m = m.min(p);
        }
        self.count += 1;
        if self.count >= self.sub_len {
            self.mins[self.slot].copy_from_slice(&self.cur_min);
            self.slot = (self.slot + 1) % NOISE_SUBWINDOWS;
            self.cur_min.fill(f32::MAX);
            self.count = 0;
        }
        for k in 0..noise.len() {
            let mut m = self.cur_min[k];
            for w in &self.mins {
                m = m.min(w[k]);
            }
            noise[k] = (m * NOISE_BIAS).min(ps[k]).max(1e-12);
        }
    }
}

fn separate_channel(x: &[f32], rate: u32, n: usize, progress: &Progress) -> Option<[Vec<f32>; 3]> {
    let hop = n / 4;
    let bins = n / 2 + 1;
    let fps = rate as f32 / hop as f32;
    let len = x.len();

    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);

    let window: Vec<f32> = (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).sqrt())
        .collect();

    // Pad so every sample is covered by full overlap.
    let mut xp = vec![0.0f32; n + len + 2 * n];
    xp[n..n + len].copy_from_slice(x);
    let frames = (xp.len() - n) / hop + 1;
    let mut outs = [
        vec![0.0f32; xp.len()],
        vec![0.0f32; xp.len()],
        vec![0.0f32; xp.len()],
    ];

    let mut buf = fwd.make_input_vec();
    let mut spec = fwd.make_output_vec();
    let mut ospec = inv.make_input_vec();
    let mut obuf = inv.make_output_vec();

    let analyse = |m: usize, buf: &mut Vec<f32>, spec: &mut Vec<Complex32>| {
        let start = m * hop;
        for i in 0..n {
            buf[i] = xp[start + i] * window[i];
        }
        fwd.process(buf, spec).ok();
    };

    let mut ps = vec![0.0f32; bins];
    let mut noise = vec![0.0f32; bins];
    let mut tracker = NoiseTracker::new(bins, fps);

    // Warm-up pass over the first seconds so the ambience floor is known from the start.
    let warm = ((NOISE_WARMUP_S * fps) as usize).min(frames);
    for m in 0..warm {
        analyse(m, &mut buf, &mut spec);
        for k in 0..bins {
            let p = spec[k].norm_sqr();
            ps[k] = if m == 0 {
                p
            } else {
                POWER_SMOOTH * ps[k] + (1.0 - POWER_SMOOTH) * p
            };
        }
        tracker.update(&ps, &mut noise);
    }

    let delay = ((REVERB_DELAY_S * fps).round() as usize).max(1);
    let delta = 3.0 * std::f32::consts::LN_10 / REVERB_T60_S;
    let rev_scale = (-2.0 * delta * delay as f32 / fps).exp();
    let mut history = vec![vec![0.0f32; bins]; delay];
    let mut hist_pos = 0;
    let mut prev_speech = vec![0.0f32; bins];
    let mut gain = vec![0.0f32; bins];
    let mut gsm = vec![0.0f32; bins];
    let mut rev = vec![0.0f32; bins];
    let bin_hz = rate as f32 / n as f32;
    let scale = 0.5 / n as f32; // overlap-add gain 2, and realfft's inverse is unnormalised

    for m in 0..frames {
        if m % 64 == 0 && progress.cancel.load(Ordering::Relaxed) {
            return None;
        }
        analyse(m, &mut buf, &mut spec);
        for k in 0..bins {
            let p = spec[k].norm_sqr();
            ps[k] = POWER_SMOOTH * ps[k] + (1.0 - POWER_SMOOTH) * p;
        }
        tracker.update(&ps, &mut noise);

        for k in 0..bins {
            let p = spec[k].norm_sqr();
            let r = rev_scale * history[hist_pos][k];
            rev[k] = r;
            let nr = noise[k] + r + 1e-12;
            let post = (p / nr - 1.0).max(0.0);
            let xi = DD_ALPHA * prev_speech[k] / nr + (1.0 - DD_ALPHA) * post;
            gain[k] = xi / (1.0 + xi);
        }
        for k in 0..bins {
            let l = gain[k.saturating_sub(1)];
            let r = gain[(k + 1).min(bins - 1)];
            let mut g = 0.25 * l + 0.5 * gain[k] + 0.25 * r;
            let f = k as f32 * bin_hz;
            if f < RUMBLE_HZ {
                g *= f / RUMBLE_HZ;
            }
            gsm[k] = g;
            prev_speech[k] = g * g * spec[k].norm_sqr();
        }
        // Store dry power for the reverb model, delay frames later.
        {
            let h = &mut history[hist_pos];
            for k in 0..bins {
                h[k] = (ps[k] - noise[k]).max(0.0);
            }
            hist_pos = (hist_pos + 1) % delay;
        }

        for (part, out) in outs.iter_mut().enumerate() {
            for k in 0..bins {
                let g = gsm[k];
                let nk = noise[k];
                let rk = rev[k];
                let mask = match part {
                    0 => g,
                    1 => (1.0 - g) * nk / (nk + rk + 1e-20),
                    _ => (1.0 - g) * rk / (nk + rk + 1e-20),
                };
                ospec[k] = spec[k] * mask;
            }
            ospec[0].im = 0.0;
            ospec[bins - 1].im = 0.0;
            inv.process(&mut ospec, &mut obuf).ok();
            let start = m * hop;
            for i in 0..n {
                out[start + i] += obuf[i] * window[i] * scale;
            }
        }
        if m % 32 == 0 {
            progress.done.fetch_add(32, Ordering::Relaxed);
        }
    }

    let [v, a, r] = outs;
    Some([
        v[n..n + len].to_vec(),
        a[n..n + len].to_vec(),
        r[n..n + len].to_vec(),
    ])
}

/// A short built-in sample: a sung-vowel "voice" with room echo and steady background noise.
pub fn demo_clip() -> Audio {
    let rate = 48_000u32;
    let len = rate as usize * 6;
    let mut voice = vec![0.0f32; len];
    let mut phase = 0.0f32;
    for i in 0..len {
        let t = i as f32 / rate as f32;
        let syll = (t * 2.2).fract();
        let env = if (t % 1.5) < 1.2 {
            (syll * std::f32::consts::PI).sin().powf(1.5)
        } else {
            0.0
        };
        let f0 = 140.0 + 30.0 * (t * 1.3).sin() + 15.0 * (t * 5.0).sin();
        phase += f0 / rate as f32;
        let formant = [(700.0, 1.0), (1200.0, 0.6), (2600.0, 0.25)];
        let mut s = 0.0;
        for h in 1..30 {
            let fh = f0 * h as f32;
            let mut a = 0.0;
            for (fc, g) in formant {
                a += g * (-((fh - fc) / 250.0).powi(2)).exp();
            }
            s += a * (2.0 * std::f32::consts::PI * phase * h as f32).sin() / (h as f32).sqrt();
        }
        voice[i] = 0.18 * env * s;
    }
    // Room echo: exponentially decaying noise burst convolved sparsely.
    let mut seed = 12345u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    let mut wet = voice.clone();
    let taps = 120;
    for tap in 0..taps {
        let d = (rate as f32 * (0.02 + 0.5 * tap as f32 / taps as f32)) as usize;
        let g = 0.25 * (-6.9 * d as f32 / rate as f32 / 0.5).exp() * rnd();
        for i in d..len {
            wet[i] += g * voice[i - d];
        }
    }
    // Background: hum plus filtered hiss.
    let mut lp = 0.0f32;
    for (i, w) in wet.iter_mut().enumerate() {
        let t = i as f32 / rate as f32;
        lp = 0.85 * lp + 0.15 * rnd();
        *w += 0.03 * lp + 0.012 * rnd() + 0.01 * (2.0 * std::f32::consts::PI * 60.0 * t).sin();
    }
    Audio {
        rate,
        channels: vec![wet],
    }
}
