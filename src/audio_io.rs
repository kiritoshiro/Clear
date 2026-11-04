//! Opening and saving audio files.

use crate::dsp::Audio;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const OPEN_EXTENSIONS: &[&str] = &[
    "wav", "wave", "aif", "aiff", "aifc", "flac", "mp3", "ogg", "oga", "m4a", "mp4", "aac", "alac",
    "caf", "mkv", "webm",
];

pub fn decode(path: &Path) -> Result<Audio, String> {
    let file = File::open(path).map_err(|e| format!("Can't open the file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("This file type isn't supported ({e})."))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No audio found in the file.")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| "This audio codec isn't supported.".to_string())?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(_)) => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("Couldn't read the file: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Couldn't decode the audio: {e}")),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let nch = spec.channels.count();
        if channels.is_empty() {
            channels = vec![Vec::new(); nch];
        }
        let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sb.copy_interleaved_ref(decoded);
        for frame in sb.samples().chunks(nch) {
            for (c, s) in frame.iter().enumerate().take(channels.len()) {
                channels[c].push(*s);
            }
        }
    }
    if channels.is_empty() || channels[0].is_empty() || rate == 0 {
        return Err("The file holds no audio.".into());
    }
    Ok(Audio { rate, channels })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportFormat {
    Wav16,
    Wav24,
    Wav32f,
    Aiff16,
    Aiff24,
    Flac16,
    Flac24,
    Mp3_320,
    Mp3_192,
    Mp3_128,
    OggVorbis,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 11] = [
        Self::Wav16,
        Self::Wav24,
        Self::Wav32f,
        Self::Aiff16,
        Self::Aiff24,
        Self::Flac16,
        Self::Flac24,
        Self::Mp3_320,
        Self::Mp3_192,
        Self::Mp3_128,
        Self::OggVorbis,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wav16 => "WAV · 16-bit",
            Self::Wav24 => "WAV · 24-bit",
            Self::Wav32f => "WAV · 32-bit float",
            Self::Aiff16 => "AIFF · 16-bit",
            Self::Aiff24 => "AIFF · 24-bit",
            Self::Flac16 => "FLAC · 16-bit",
            Self::Flac24 => "FLAC · 24-bit",
            Self::Mp3_320 => "MP3 · 320 kbps",
            Self::Mp3_192 => "MP3 · 192 kbps",
            Self::Mp3_128 => "MP3 · 128 kbps",
            Self::OggVorbis => "OGG Vorbis · high",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav16 | Self::Wav24 | Self::Wav32f => "wav",
            Self::Aiff16 | Self::Aiff24 => "aiff",
            Self::Flac16 | Self::Flac24 => "flac",
            Self::Mp3_320 | Self::Mp3_192 | Self::Mp3_128 => "mp3",
            Self::OggVorbis => "ogg",
        }
    }
}

fn clamp(s: f32) -> f32 {
    s.clamp(-1.0, 1.0)
}

fn to_int(s: f32, bits: u32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    (clamp(s) * max).round() as i32
}

pub fn export(path: &Path, fmt: ExportFormat, audio: &Audio) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("Couldn't save: {e}");
    match fmt {
        ExportFormat::Wav16 | ExportFormat::Wav24 | ExportFormat::Wav32f => {
            let float = fmt == ExportFormat::Wav32f;
            let bits = match fmt {
                ExportFormat::Wav16 => 16,
                ExportFormat::Wav24 => 24,
                _ => 32,
            };
            let spec = hound::WavSpec {
                channels: audio.channels.len() as u16,
                sample_rate: audio.rate,
                bits_per_sample: bits,
                sample_format: if float {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
            };
            let mut w = hound::WavWriter::create(path, spec).map_err(|e| err(&e))?;
            for i in 0..audio.len() {
                for ch in &audio.channels {
                    if float {
                        w.write_sample(clamp(ch[i])).map_err(|e| err(&e))?;
                    } else {
                        w.write_sample(to_int(ch[i], bits as u32))
                            .map_err(|e| err(&e))?;
                    }
                }
            }
            w.finalize().map_err(|e| err(&e))
        }
        ExportFormat::Aiff16 | ExportFormat::Aiff24 => write_aiff(
            path,
            audio,
            if fmt == ExportFormat::Aiff16 { 16 } else { 24 },
        )
        .map_err(|e| err(&e)),
        ExportFormat::Flac16 | ExportFormat::Flac24 => {
            let bits = if fmt == ExportFormat::Flac16 { 16 } else { 24 };
            write_flac(path, audio, bits)
        }
        ExportFormat::Mp3_320 | ExportFormat::Mp3_192 | ExportFormat::Mp3_128 => {
            let rate = match fmt {
                ExportFormat::Mp3_320 => mp3lame_encoder::Bitrate::Kbps320,
                ExportFormat::Mp3_192 => mp3lame_encoder::Bitrate::Kbps192,
                _ => mp3lame_encoder::Bitrate::Kbps128,
            };
            write_mp3(path, audio, rate)
        }
        ExportFormat::OggVorbis => write_ogg(path, audio),
    }
}

fn write_aiff(path: &Path, audio: &Audio, bits: u32) -> std::io::Result<()> {
    let nch = audio.channels.len() as u16;
    let frames = audio.len() as u32;
    let bytes = bits / 8;
    let data_len = frames * nch as u32 * bytes;
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"FORM")?;
    w.write_all(&(4 + 26 + 16 + data_len).to_be_bytes())?;
    w.write_all(b"AIFF")?;
    w.write_all(b"COMM")?;
    w.write_all(&18u32.to_be_bytes())?;
    w.write_all(&nch.to_be_bytes())?;
    w.write_all(&frames.to_be_bytes())?;
    w.write_all(&(bits as u16).to_be_bytes())?;
    w.write_all(&extended80(audio.rate))?;
    w.write_all(b"SSND")?;
    w.write_all(&(8 + data_len).to_be_bytes())?;
    w.write_all(&0u32.to_be_bytes())?;
    w.write_all(&0u32.to_be_bytes())?;
    for i in 0..audio.len() {
        for ch in &audio.channels {
            let v = to_int(ch[i], bits).to_be_bytes();
            w.write_all(&v[4 - bytes as usize..])?;
        }
    }
    if data_len % 2 == 1 {
        w.write_all(&[0])?;
    }
    w.flush()
}

/// Sample rate as an 80-bit IEEE extended float, as AIFF wants it.
fn extended80(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let e = 31 - rate.leading_zeros();
    let mant = (rate as u64) << (63 - e);
    out[..2].copy_from_slice(&((16383 + e) as u16).to_be_bytes());
    out[2..].copy_from_slice(&mant.to_be_bytes());
    out
}

fn write_flac(path: &Path, audio: &Audio, bits: u32) -> Result<(), String> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    let nch = audio.channels.len();
    let mut samples = Vec::with_capacity(audio.len() * nch);
    for i in 0..audio.len() {
        for ch in &audio.channels {
            samples.push(to_int(ch[i], bits));
        }
    }
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("FLAC setup failed: {e:?}"))?;
    let source =
        flacenc::source::MemSource::from_samples(&samples, nch, bits as usize, audio.rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("FLAC encoding failed: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("FLAC encoding failed: {e:?}"))?;
    let mut bytes = sink.as_slice().to_vec();
    // flacenc stores the short final block's length as the minimum block size, but a fixed
    // block-size stream must have min == max (strict decoders reject it otherwise).
    if bytes.len() > 12 && &bytes[..4] == b"fLaC" && bytes[4] & 0x7f == 0 {
        let (min, max) = (8, 10);
        bytes.copy_within(max..max + 2, min);
    }
    std::fs::write(path, bytes).map_err(|e| format!("Couldn't save: {e}"))
}

fn write_mp3(path: &Path, audio: &Audio, bitrate: mp3lame_encoder::Bitrate) -> Result<(), String> {
    use mp3lame_encoder::{Builder, DualPcm, FlushNoGap, MonoPcm};
    let e = |x: &dyn std::fmt::Display| format!("MP3 encoding failed: {x}");
    let mono = audio.channels.len() == 1;
    let mut b = Builder::new().ok_or("MP3 encoder unavailable")?;
    b.set_num_channels(if mono { 1 } else { 2 })
        .map_err(|x| e(&x))?;
    b.set_sample_rate(audio.rate).map_err(|x| e(&x))?;
    if audio.rate > 48_000 {
        b.set_output_sample_rate(NonZeroU32::new(48_000))
            .map_err(|x| e(&x))?;
    }
    b.set_brate(bitrate).map_err(|x| e(&x))?;
    b.set_quality(mp3lame_encoder::Quality::Best)
        .map_err(|x| e(&x))?;
    let mut enc = b.build().map_err(|x| e(&x))?;

    let left: Vec<f32> = audio.channels[0].iter().map(|s| clamp(*s)).collect();
    let right: Vec<f32> = if mono {
        Vec::new()
    } else {
        audio.channels[1].iter().map(|s| clamp(*s)).collect()
    };
    let mut out = Vec::new();
    for start in (0..left.len()).step_by(65_536) {
        let end = (start + 65_536).min(left.len());
        out.reserve(mp3lame_encoder::max_required_buffer_size(end - start));
        if mono {
            enc.encode_to_vec(MonoPcm(&left[start..end]), &mut out)
                .map_err(|x| e(&x))?;
        } else {
            let pcm = DualPcm {
                left: &left[start..end],
                right: &right[start..end],
            };
            enc.encode_to_vec(pcm, &mut out).map_err(|x| e(&x))?;
        }
    }
    out.reserve(7200);
    enc.flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|x| e(&x))?;
    std::fs::write(path, out).map_err(|x| format!("Couldn't save: {x}"))
}

fn write_ogg(path: &Path, audio: &Audio) -> Result<(), String> {
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};
    let e = |x: &dyn std::fmt::Display| format!("OGG encoding failed: {x}");
    let file = BufWriter::new(File::create(path).map_err(|x| format!("Couldn't save: {x}"))?);
    let rate = NonZeroU32::new(audio.rate).ok_or("bad sample rate")?;
    let nch = NonZeroU8::new(audio.channels.len().min(255) as u8).ok_or("no channels")?;
    let mut builder = VorbisEncoderBuilder::new(rate, nch, file).map_err(|x| e(&x))?;
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: 0.6,
    });
    let mut enc = builder.build().map_err(|x| e(&x))?;
    for start in (0..audio.len()).step_by(4096) {
        let end = (start + 4096).min(audio.len());
        let block: Vec<Vec<f32>> = audio
            .channels
            .iter()
            .map(|c| c[start..end].iter().map(|s| clamp(*s)).collect())
            .collect();
        enc.encode_audio_block(&block).map_err(|x| e(&x))?;
    }
    enc.finish()
        .map_err(|x| e(&x))?
        .flush()
        .map_err(|x| format!("Couldn't save: {x}"))
}
