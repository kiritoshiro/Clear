//! Live playback of the cleaned mix (or the untouched original) through the default output.

use crate::dsp::{Audio, Stems};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct PlayState {
    pub original: Option<Arc<Audio>>,
    pub stems: Option<Arc<Stems>>,
    pub gains: [f32; 3], // voice, ambience, reverb
    pub listen_original: bool,
    pub playing: bool,
    pub pos: f64, // in source frames
    pub peak: f32,
}

pub struct Player {
    pub state: Arc<Mutex<PlayState>>,
    _stream: Option<cpal::Stream>,
    pub error: Option<String>,
}

impl Player {
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(PlayState::default()));
        match start_stream(state.clone()) {
            Ok(s) => Self {
                state,
                _stream: Some(s),
                error: None,
            },
            Err(e) => Self {
                state,
                _stream: None,
                error: Some(e),
            },
        }
    }
}

fn start_stream(state: Arc<Mutex<PlayState>>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("No sound output found")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let out_ch = config.channels as usize;
    let out_rate = config.sample_rate as f64;
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| fill(&state, data, out_ch, out_rate),
            |_| {},
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn fill(state: &Mutex<PlayState>, data: &mut [f32], out_ch: usize, out_rate: f64) {
    data.fill(0.0);
    let Ok(mut st) = state.try_lock() else { return };
    if !st.playing {
        return;
    }
    let Some(orig) = st.original.clone() else {
        return;
    };
    let stems = st.stems.clone();
    let len = orig.len();
    let step = orig.rate as f64 / out_rate;
    let [gv, ga, gr] = st.gains;
    let use_orig = st.listen_original || stems.is_none();
    let src_ch = orig.channels.len();
    let mut pos = st.pos;
    let mut peak = 0.0f32;
    for frame in data.chunks_mut(out_ch) {
        let i = pos as usize;
        if i + 1 >= len {
            st.playing = false;
            pos = 0.0;
            break;
        }
        let t = (pos - i as f64) as f32;
        for (oc, out) in frame.iter_mut().enumerate() {
            let c = if src_ch == 1 {
                0
            } else if oc < src_ch {
                oc
            } else {
                continue;
            };
            let sample = |buf: &[f32]| buf[i] + (buf[i + 1] - buf[i]) * t;
            let v = if use_orig {
                sample(&orig.channels[c])
            } else {
                let s = stems.as_ref().unwrap();
                gv * sample(&s.voice[c]) + ga * sample(&s.ambience[c]) + gr * sample(&s.reverb[c])
            };
            *out = v.clamp(-1.0, 1.0);
            peak = peak.max(v.abs());
        }
        pos += step;
    }
    st.pos = pos;
    st.peak = st.peak.max(peak);
}
