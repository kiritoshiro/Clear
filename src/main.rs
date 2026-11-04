#![windows_subsystem = "windows"]

mod audio_io;
mod dsp;
mod player;

use audio_io::ExportFormat;
use dsp::{Audio, Progress, Stems};
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

const BG: Color32 = Color32::from_rgb(0x11, 0x12, 0x16);
const CARD: Color32 = Color32::from_rgb(0x1a, 0x1c, 0x22);
const CARD_EDGE: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x36);
const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x8f, 0x9c);
const AMBIENCE: Color32 = Color32::from_rgb(0x4f, 0xd1, 0xc5);
const VOICE: Color32 = Color32::from_rgb(0x6e, 0xa8, 0xfe);
const REVERB: Color32 = Color32::from_rgb(0xb3, 0x88, 0xff);
const WAVE_ORIG: Color32 = Color32::from_rgb(0x3a, 0x3e, 0x4a);

enum Job {
    Loading(Receiver<Result<(Audio, String), String>>),
    Cleaning(Arc<Progress>, Receiver<Option<Stems>>),
    Exporting(Receiver<Result<PathBuf, String>>),
}

struct App {
    player: player::Player,
    audio: Option<Arc<Audio>>,
    stems: Option<Arc<Stems>>,
    name: String,
    source_path: Option<PathBuf>,
    job: Option<Job>,
    voice_db: f32,
    ambience_db: f32,
    reverb_db: f32,
    format: ExportFormat,
    status: String,
    orig_peaks: Vec<(f32, f32)>,
    mix_peaks: Vec<(f32, f32)>,
    peaks_for: Option<[f32; 3]>,
    screenshot: Option<PathBuf>,
    frames: u32,
    shot_rx: Option<Receiver<Arc<egui::ColorImage>>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<PathBuf>) -> Self {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x26, 0x29, 0x31);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x30, 0x34, 0x3e);
        v.selection.bg_fill = VOICE.linear_multiply(0.6);
        cc.egui_ctx.set_visuals(v);
        let mut app = Self {
            player: player::Player::new(),
            audio: None,
            stems: None,
            name: String::new(),
            source_path: None,
            job: None,
            voice_db: dsp::DEFAULT_VOICE_DB,
            ambience_db: dsp::DEFAULT_AMBIENCE_DB,
            reverb_db: dsp::DEFAULT_REVERB_DB,
            format: ExportFormat::Wav24,
            status: "Open a recording, or try the built-in demo clip.".into(),
            orig_peaks: vec![],
            mix_peaks: vec![],
            peaks_for: None,
            screenshot,
            frames: 0,
            shot_rx: None,
        };
        app.set_audio(dsp::demo_clip(), "Demo clip (built-in)".into(), None);
        app
    }

    fn gains(&self) -> [f32; 3] {
        [
            dsp::db_to_gain(self.voice_db),
            dsp::db_to_gain(self.ambience_db),
            dsp::db_to_gain(self.reverb_db),
        ]
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn open_file(&mut self, path: PathBuf) {
        if let Some(Job::Cleaning(p, _)) = &self.job {
            p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let (tx, rx) = channel();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.status = format!("Opening {name}…");
        self.source_path = Some(path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(audio_io::decode(&path).map(|a| (a, name)));
        });
        self.job = Some(Job::Loading(rx));
    }

    fn set_audio(&mut self, audio: Audio, name: String, path: Option<PathBuf>) {
        let audio = Arc::new(audio);
        {
            let mut st = self.player.state.lock().unwrap();
            st.playing = false;
            st.pos = 0.0;
            st.original = Some(audio.clone());
            st.stems = None;
        }
        self.orig_peaks = peaks(&audio.channels, audio.len(), 900);
        self.mix_peaks.clear();
        self.peaks_for = None;
        self.stems = None;
        self.name = name;
        if path.is_some() {
            self.source_path = path;
        }
        let progress = Progress::new();
        let (tx, rx) = channel();
        let (a, p) = (audio.clone(), progress.clone());
        std::thread::spawn(move || {
            let _ = tx.send(dsp::separate(&a, &p));
        });
        self.status = "Separating voice, ambience and reverb…".into();
        self.job = Some(Job::Cleaning(progress, rx));
        self.audio = Some(audio);
    }

    fn poll_jobs(&mut self) {
        let Some(job) = self.job.take() else { return };
        match job {
            Job::Loading(rx) => match rx.try_recv() {
                Ok(Ok((audio, name))) => self.set_audio(audio, name, None),
                Ok(Err(e)) => self.status = e,
                Err(std::sync::mpsc::TryRecvError::Empty) => self.job = Some(Job::Loading(rx)),
                Err(_) => self.status = "Opening failed.".into(),
            },
            Job::Cleaning(p, rx) => match rx.try_recv() {
                Ok(Some(stems)) => {
                    let stems = Arc::new(stems);
                    self.player.state.lock().unwrap().stems = Some(stems.clone());
                    self.stems = Some(stems);
                    self.peaks_for = None;
                    self.status = "Ready. Turn the knobs and press Play to listen.".into();
                }
                Ok(None) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => self.job = Some(Job::Cleaning(p, rx)),
                Err(_) => self.status = "Cleaning failed.".into(),
            },
            Job::Exporting(rx) => match rx.try_recv() {
                Ok(Ok(path)) => self.status = format!("Saved {}", path.display()),
                Ok(Err(e)) => self.status = e,
                Err(std::sync::mpsc::TryRecvError::Empty) => self.job = Some(Job::Exporting(rx)),
                Err(_) => self.status = "Saving failed.".into(),
            },
        }
    }

    fn export(&mut self) {
        let (Some(audio), Some(stems)) = (self.audio.clone(), self.stems.clone()) else {
            return;
        };
        let fmt = self.format;
        let stem = self
            .source_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "demo".into());
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(format!("{stem} (clean).{}", fmt.extension()))
            .add_filter(fmt.label(), &[fmt.extension()]);
        if let Some(dir) = self.source_path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(fmt.extension());
        }
        let [gv, ga, gr] = self.gains();
        let (tx, rx) = channel();
        self.status = format!("Saving {}…", fmt.label());
        std::thread::spawn(move || {
            let out = Audio {
                rate: audio.rate,
                channels: stems.mix(gv, ga, gr),
            };
            let _ = tx.send(audio_io::export(&path, fmt, &out).map(|_| path));
        });
        self.job = Some(Job::Exporting(rx));
    }

    fn refresh_mix_peaks(&mut self) {
        let g = self.gains();
        if self.peaks_for == Some(g) {
            return;
        }
        if let (Some(a), Some(s)) = (&self.audio, &self.stems) {
            let mixed: Vec<Vec<f32>> = vec![
                (0..a.len())
                    .map(|i| g[0] * s.voice[0][i] + g[1] * s.ambience[0][i] + g[2] * s.reverb[0][i])
                    .collect(),
            ];
            self.mix_peaks = peaks(&mixed, a.len(), 900);
            self.peaks_for = Some(g);
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        self.frames += 1;
        ctx.request_repaint();
        if let Some(rx) = &self.shot_rx {
            if let Ok(img) = rx.try_recv() {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                    let _ = buf.save(&path);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let ready = !self.busy() || self.frames > 900;
        if self.frames >= 120 && ready {
            let (tx, rx) = channel();
            let tx = Mutex::new(tx);
            ctx.request_screenshot(move |img| {
                let _ = tx.lock().unwrap().send(img);
            });
            self.shot_rx = Some(rx);
        }
    }
}

fn peaks(chs: &[Vec<f32>], len: usize, cols: usize) -> Vec<(f32, f32)> {
    if len == 0 {
        return vec![];
    }
    (0..cols)
        .map(|c| {
            let a = c * len / cols;
            let b = ((c + 1) * len / cols).max(a + 1).min(len);
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for ch in chs {
                for &s in &ch[a..b] {
                    lo = lo.min(s);
                    hi = hi.max(s);
                }
            }
            (lo, hi)
        })
        .collect()
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, CARD_EDGE))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

fn fmt_db(db: f32) -> String {
    if db <= dsp::KNOB_MIN_DB {
        "Off".into()
    } else {
        format!("{db:+.1} dB")
    }
}

fn fmt_time(s: f32) -> String {
    let s = s.max(0.0);
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

/// A round level knob: drag up/down or scroll to change, double-click to reset.
fn knob(ui: &mut egui::Ui, value: &mut f32, label: &str, color: Color32, default: f32) -> bool {
    let size = Vec2::new(150.0, 190.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let before = *value;
    let span = dsp::KNOB_MAX_DB - dsp::KNOB_MIN_DB;
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.2
        } else {
            1.0
        };
        *value -= resp.drag_delta().y * span / 220.0 * fine;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        *value += scroll * 0.05;
    }
    if resp.double_clicked() {
        *value = default;
    }
    *value = value.clamp(dsp::KNOB_MIN_DB, dsp::KNOB_MAX_DB);

    let p = ui.painter();
    let center = Pos2::new(rect.center().x, rect.top() + 72.0);
    let r = 52.0;
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let frac = (*value - dsp::KNOB_MIN_DB) / span;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let steps = 64;
        (0..=steps)
            .map(|i| {
                let a = start + sweep * (from + (to - from) * i as f32 / steps as f32);
                center + Vec2::new(a.cos(), a.sin()) * r
            })
            .collect()
    };
    p.circle_filled(center, r + 10.0, Color32::from_rgb(0x15, 0x16, 0x1b));
    p.line(
        arc(0.0, 1.0),
        Stroke::new(6.0, Color32::from_rgb(0x2c, 0x2f, 0x39)),
    );
    if frac > 0.001 {
        p.line(arc(0.0, frac), Stroke::new(6.0, color));
    }
    let zero = (0.0 - dsp::KNOB_MIN_DB) / span;
    let za = start + sweep * zero;
    p.line_segment(
        [
            center + Vec2::new(za.cos(), za.sin()) * (r + 6.0),
            center + Vec2::new(za.cos(), za.sin()) * (r + 12.0),
        ],
        Stroke::new(2.0, TEXT_DIM),
    );
    let body = if resp.hovered() || resp.dragged() {
        Color32::from_rgb(0x2e, 0x32, 0x3c)
    } else {
        Color32::from_rgb(0x25, 0x28, 0x30)
    };
    p.circle_filled(center, r - 12.0, body);
    p.circle_stroke(center, r - 12.0, Stroke::new(1.0, CARD_EDGE));
    let a = start + sweep * frac;
    p.line_segment(
        [
            center + Vec2::new(a.cos(), a.sin()) * (r - 32.0),
            center + Vec2::new(a.cos(), a.sin()) * (r - 16.0),
        ],
        Stroke::new(3.0, color),
    );
    p.text(
        Pos2::new(center.x, rect.top() + 146.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(16.0),
        Color32::WHITE,
    );
    p.text(
        Pos2::new(center.x, rect.top() + 170.0),
        egui::Align2::CENTER_CENTER,
        fmt_db(*value),
        egui::FontId::monospace(14.0),
        color,
    );
    resp.on_hover_text("Drag up/down or scroll. Shift for fine steps. Double-click to reset.");
    before != *value
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_jobs();
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(p) = dropped.into_iter().next() {
            self.open_file(p);
        }

        let (playing, pos, peak) = {
            let mut st = self.player.state.lock().unwrap();
            st.gains = self.gains();
            let pk = st.peak;
            st.peak *= 0.85;
            (st.playing, st.pos, pk)
        };
        if playing {
            ctx.request_repaint();
        }
        self.refresh_mix_peaks();

        egui::Panel::top("top")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 12.0, VOICE);
                    ui.painter().circle_filled(r.center(), 6.0, BG);
                    ui.label(
                        RichText::new("Voice Cleanup")
                            .size(20.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.add_space(12.0);
                    ui.label(RichText::new("Ambience · Voice · Voice Reverb").color(TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Open audio…").size(15.0))
                                    .min_size(Vec2::new(120.0, 32.0)),
                            )
                            .clicked()
                        {
                            let exts: Vec<&str> = audio_io::OPEN_EXTENSIONS.to_vec();
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("Audio", &exts)
                                .pick_file()
                            {
                                self.open_file(p);
                            }
                        }
                    });
                });
            });

        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(20, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    match &self.job {
                        Some(Job::Cleaning(p, _)) => {
                            ui.add(
                                egui::ProgressBar::new(p.fraction())
                                    .desired_width(180.0)
                                    .show_percentage(),
                            );
                        }
                        Some(_) => {
                            ui.spinner();
                        }
                        None => {}
                    }
                    ui.label(RichText::new(&self.status).color(TEXT_DIM));
                    if let Some(e) = &self.player.error {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("Sound output: {e}"))
                                    .color(Color32::from_rgb(0xe0, 0x7a, 0x5f)),
                            );
                        });
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(20, 4)),
            )
            .show(ui, |ui| {
                // Waveform card
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&self.name).size(15.0).color(Color32::WHITE));
                        if let Some(a) = &self.audio {
                            ui.label(
                                RichText::new(format!(
                                    "{} Hz · {} · {}",
                                    a.rate,
                                    match a.channels.len() {
                                        1 => "mono".to_string(),
                                        2 => "stereo".to_string(),
                                        n => format!("{n} ch"),
                                    },
                                    fmt_time(a.seconds())
                                ))
                                .color(TEXT_DIM),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for (name, col) in [
                                ("cleaned", VOICE),
                                ("original", Color32::from_rgb(0x5a, 0x5f, 0x6c)),
                            ] {
                                ui.label(RichText::new(name).color(col));
                                let (r, _) =
                                    ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                ui.painter().rect_filled(r, CornerRadius::same(2), col);
                                ui.add_space(6.0);
                            }
                        });
                    });
                    ui.add_space(8.0);
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 170.0),
                        Sense::click_and_drag(),
                    );
                    let p = ui.painter_at(rect);
                    p.rect_filled(
                        rect,
                        CornerRadius::same(8),
                        Color32::from_rgb(0x13, 0x14, 0x19),
                    );
                    let mid = rect.center().y;
                    let hh = rect.height() * 0.46;
                    p.line_segment(
                        [Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)],
                        Stroke::new(1.0, CARD_EDGE),
                    );
                    let draw = |pk: &[(f32, f32)], color: Color32| {
                        let n = pk.len().max(1) as f32;
                        for (i, (lo, hi)) in pk.iter().enumerate() {
                            let x = rect.left() + (i as f32 + 0.5) * rect.width() / n;
                            let y0 = mid - hi.clamp(-1.0, 1.0) * hh;
                            let y1 = mid - lo.clamp(-1.0, 1.0) * hh;
                            p.line_segment(
                                [Pos2::new(x, y0), Pos2::new(x, y1.max(y0 + 1.0))],
                                Stroke::new(1.2, color),
                            );
                        }
                    };
                    draw(&self.orig_peaks, WAVE_ORIG);
                    if self.stems.is_some() {
                        draw(&self.mix_peaks, VOICE.linear_multiply(0.85));
                    }
                    if let Some(a) = &self.audio {
                        let len = a.len().max(1) as f32;
                        let x = rect.left() + (pos as f32 / len) * rect.width();
                        p.line_segment(
                            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                            Stroke::new(2.0, Color32::WHITE),
                        );
                        if let Some(click) = resp.interact_pointer_pos() {
                            let f = ((click.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                            self.player.state.lock().unwrap().pos = (f * len) as f64;
                        }
                    }
                });

                ui.add_space(10.0);
                // Transport
                ui.horizontal(|ui| {
                    let can_play = self.audio.is_some();
                    let label = if playing { "Pause" } else { "Play" };
                    if ui
                        .add_enabled(
                            can_play,
                            egui::Button::new(RichText::new(label).size(15.0))
                                .min_size(Vec2::new(110.0, 34.0)),
                        )
                        .clicked()
                    {
                        let mut st = self.player.state.lock().unwrap();
                        st.playing = !st.playing;
                    }
                    if ui
                        .add_enabled(
                            can_play,
                            egui::Button::new(RichText::new("Stop").size(15.0))
                                .min_size(Vec2::new(90.0, 34.0)),
                        )
                        .clicked()
                    {
                        let mut st = self.player.state.lock().unwrap();
                        st.playing = false;
                        st.pos = 0.0;
                    }
                    ui.add_space(12.0);
                    let mut orig = self.player.state.lock().unwrap().listen_original;
                    ui.label(RichText::new("Listen:").color(TEXT_DIM));
                    ui.selectable_value(&mut orig, false, RichText::new("Cleaned").size(14.0));
                    ui.selectable_value(&mut orig, true, RichText::new("Original").size(14.0));
                    self.player.state.lock().unwrap().listen_original = orig;
                    if let Some(a) = &self.audio {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(format!(
                                "{} / {}",
                                fmt_time(pos as f32 / a.rate as f32),
                                fmt_time(a.seconds())
                            ))
                            .monospace()
                            .color(TEXT_DIM),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(140.0, 10.0), Sense::hover());
                        ui.painter().rect_filled(
                            r,
                            CornerRadius::same(3),
                            Color32::from_rgb(0x26, 0x29, 0x31),
                        );
                        let w = (peak.min(1.0)) * r.width();
                        let col = if peak >= 1.0 {
                            Color32::from_rgb(0xe0, 0x5f, 0x5f)
                        } else {
                            AMBIENCE
                        };
                        ui.painter().rect_filled(
                            Rect::from_min_size(r.min, Vec2::new(w, r.height())),
                            CornerRadius::same(3),
                            col,
                        );
                        ui.label(RichText::new("Level").color(TEXT_DIM));
                    });
                });

                ui.add_space(10.0);
                // Knobs
                card(ui, |ui| {
                    let total = 3.0 * 150.0 + 2.0 * 60.0;
                    ui.horizontal(|ui| {
                        ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                        knob(
                            ui,
                            &mut self.ambience_db,
                            "Ambience",
                            AMBIENCE,
                            dsp::DEFAULT_AMBIENCE_DB,
                        );
                        ui.add_space(60.0);
                        knob(
                            ui,
                            &mut self.voice_db,
                            "Voice",
                            VOICE,
                            dsp::DEFAULT_VOICE_DB,
                        );
                        ui.add_space(60.0);
                        knob(
                            ui,
                            &mut self.reverb_db,
                            "Voice Reverb",
                            REVERB,
                            dsp::DEFAULT_REVERB_DB,
                        );
                    });
                });

                ui.add_space(10.0);
                // Export
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Save cleaned audio as").color(TEXT_DIM));
                    egui::ComboBox::from_id_salt("fmt")
                        .width(180.0)
                        .selected_text(self.format.label())
                        .show_ui(ui, |ui| {
                            for f in ExportFormat::ALL {
                                ui.selectable_value(&mut self.format, f, f.label());
                            }
                        });
                    let ready = self.stems.is_some() && !self.busy();
                    let btn = egui::Button::new(
                        RichText::new("Export…").size(15.0).color(Color32::WHITE),
                    )
                    .fill(VOICE.linear_multiply(0.55))
                    .min_size(Vec2::new(110.0, 32.0));
                    if ui.add_enabled(ready, btn).clicked() {
                        self.export();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset knobs").clicked() {
                            self.voice_db = dsp::DEFAULT_VOICE_DB;
                            self.ambience_db = dsp::DEFAULT_AMBIENCE_DB;
                            self.reverb_db = dsp::DEFAULT_REVERB_DB;
                        }
                    });
                });
            });

        self.handle_screenshot(&ctx);
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--selftest") {
        let dir = PathBuf::from(
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| "selftest".into()),
        );
        selftest(&dir).expect("audio self-test failed; see report.txt");
        return Ok(());
    }
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([760.0, 600.0])
            .with_title("Voice Cleanup")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Voice Cleanup",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, screenshot)))),
    )
}

/// Cleans the demo clip, saves it in every format and reads each file back; writes report.txt.
fn selftest(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut failures = 0;
    let mut report = String::new();
    let audio = dsp::demo_clip();
    let t = std::time::Instant::now();
    let stems = dsp::separate(&audio, &Progress::new()).expect("separation");
    report += &format!(
        "separate: {:.2}s for {:.1}s audio
",
        t.elapsed().as_secs_f32(),
        audio.seconds()
    );
    let unity = stems.mix(1.0, 1.0, 1.0);
    let err = unity[0]
        .iter()
        .zip(&audio.channels[0])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    report += &format!(
        "all knobs 0 dB, max difference from input: {err:.6}
"
    );
    if !err.is_finite() || err > 0.0001 || unity[0].iter().any(|s| !s.is_finite()) {
        failures += 1;
        report += "FAIL unity mix does not reconstruct the input\n";
    }
    let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
    report += &format!(
        "rms input {:.4} voice {:.4} ambience {:.4} reverb {:.4}
",
        rms(&audio.channels[0]),
        rms(&stems.voice[0]),
        rms(&stems.ambience[0]),
        rms(&stems.reverb[0])
    );
    let clean = Audio {
        rate: audio.rate,
        channels: stems.mix(1.0, 0.0, 0.0),
    };
    let stereo = Audio {
        rate: 44_100,
        channels: vec![clean.channels[0].clone(), clean.channels[0].clone()],
    };
    for (tag, a) in [("mono", &clean), ("stereo", &stereo)] {
        for f in ExportFormat::ALL {
            let path = dir.join(format!("{tag}_{:?}.{}", f, f.extension()));
            let line = match audio_io::export(&path, f, a) {
                Ok(()) => match audio_io::decode(&path) {
                    Ok(b)
                        if b.rate == a.rate
                            && b.channels.len() == a.channels.len()
                            && (b.seconds() - a.seconds()).abs() < 0.2
                            && b.channels.iter().flatten().all(|s| s.is_finite()) =>
                    {
                        format!(
                            "ok   {:?} {tag}: {} Hz, {} ch, {:.2}s",
                            f,
                            b.rate,
                            b.channels.len(),
                            b.seconds()
                        )
                    }
                    Ok(_) => format!(
                        "FAIL {:?} {tag}: decoded audio has unexpected dimensions or samples",
                        f
                    ),
                    Err(e) => format!("FAIL {:?} {tag} read back: {e}", f),
                },
                Err(e) => format!("FAIL {:?} {tag} save: {e}", f),
            };
            if line.starts_with("FAIL") {
                failures += 1;
            }
            report += &line;
            report += "
";
        }
    }
    // Files from other programs placed in the folder as in_*.
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("in_"))
        {
            let line = match audio_io::decode(&path)
                .map(|a| (dsp::separate(&a, &Progress::new()).is_some(), a))
            {
                Ok((true, a)) => format!(
                    "ok   open {}: {} Hz, {} ch, {:.2}s, cleaned true\n",
                    path.display(),
                    a.rate,
                    a.channels.len(),
                    a.seconds()
                ),
                Ok((false, _)) => format!("FAIL open {}: separation failed\n", path.display()),
                Err(e) => format!("FAIL open {}: {e}\n", path.display()),
            };
            if line.starts_with("FAIL") {
                failures += 1;
            }
            report += &line;
        }
    }
    std::fs::write(dir.join("report.txt"), &report).map_err(|e| e.to_string())?;
    if failures > 0 { Err(report) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dsp_and_audio_formats_roundtrip() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("clear-selftest-{}-{stamp}", std::process::id()));
        super::selftest(&dir).unwrap_or_else(|e| panic!("{}\n{e}", dir.display()));
        std::fs::remove_dir_all(&dir).expect("remove generated self-test files");
    }
}
