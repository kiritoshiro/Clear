# Clear rebuild — mechanics sheet

## What the evidence shows

| Fact | Value | Source |
|---|---|---|
| `Clear.vst3` is only a loader | `GetPluginFactory` calls `LoadLibraryA` on a second file, patches the process, then forwards `GetPluginFactory` | function 0x180007594 (Clear.vst3), evidence ev_5f67680d… |
| The real plugin | `Clear.bin`, product "Clear", file version 1.1.1, publisher string "Supertone Inc." | version resource, evidence ev_bf2045e7… |
| Processing code | not readable: the code section `.]MP` is encrypted/packed (entropy 7.98 bits/byte); no parameter names, ranges or model data are visible as text | triage ev_8c33dc0b…, string scan of Clear.bin |

The plugin's protection was not removed, so none of the original's DSP constants could be read.
Everything below is **estimated**: an original design that gives the same three-control
behaviour (split a recording into voice, ambience and voice-reverb and set each one's level).

## Controls (estimated)

| Control | Range | Default |
|---|---|---|
| Ambience | Off (≤ -60 dB) … +12 dB | Off |
| Voice | Off (≤ -60 dB) … +12 dB | 0 dB |
| Voice Reverb | Off (≤ -60 dB) … +12 dB | Off |

Gain: `g = 0` when dB ≤ -60, else `10^(dB/20)`. Double-click a knob to reset it.

## Separation engine (estimated, own design)

Short-time Fourier transform per channel.

* Frame 2048 samples at 48 kHz (scaled by the nearest power of two of rate/48000, min 1024), hop = frame/4.
* Window: square-root Hann for analysis and synthesis; overlap-add gain 2, so output × 0.5.
* Smoothed power: `Ps = 0.7·Ps_prev + 0.3·|X|²`.
* Ambience (noise) floor: minimum statistics over 1.5 s (8 sub-windows), bias 1.6.
  The first 3 s are run through the tracker once before the real pass (warm-up).
* Voice reverb (late reverberation, Lebart model): `R = e^(-2·δ·Td) · max(Ps − N, 0)` taken `Td`
  earlier, `δ = 3·ln10 / T60`, `T60 = 0.5 s`, `Td = 0.048 s`.
* Voice: decision-directed a-priori SNR, `ξ = 0.92·Ŝ_prev/(N+R) + 0.08·max(|X|²/(N+R) − 1, 0)`,
  Wiener gain `G = ξ/(1+ξ)`, smoothed across frequency with [0.25, 0.5, 0.25];
  below 80 Hz `G` is scaled by `f/80` (rumble counts as ambience).
* Masks: voice `G`, ambience `(1−G)·N/(N+R)`, reverb `(1−G)·R/(N+R)`; they add up to 1,
  so with all three knobs at 0 dB the output equals the input.
* The three parts are rendered once; the knobs mix them live:
  `out = g_voice·voice + g_ambience·ambience + g_reverb·reverb`.

## Files (from the person's request)

* Open: WAV, AIFF, FLAC, MP3, OGG Vorbis, M4A/AAC, ALAC, CAF, MKV/WebM audio.
* Save: WAV 16/24-bit and 32-bit float, AIFF 16/24-bit, FLAC 16/24-bit,
  MP3 320/192/128 kbps (LAME, stereo max, ≤48 kHz), OGG Vorbis quality 0.6.
* Samples are clamped to −1…+1 on save.
