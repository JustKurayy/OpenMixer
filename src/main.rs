#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};
use openmixer::{
    audio::{AudioEngine, AudioStatus, DeviceList, TelemetrySnapshot},
    dsp::{DspControls, MeterBand, dba_meter_band, estimated_dba},
};

const BG: Color32 = Color32::from_rgb(12, 14, 15);
const STRIP: Color32 = Color32::from_rgb(19, 22, 23);
const RAISED: Color32 = Color32::from_rgb(27, 30, 31);
const LINE: Color32 = Color32::from_rgb(48, 52, 53);
const TEXT: Color32 = Color32::from_rgb(205, 208, 202);
const MUTED: Color32 = Color32::from_rgb(104, 111, 108);
const CYAN: Color32 = Color32::from_rgb(71, 190, 180);
const GREEN: Color32 = Color32::from_rgb(42, 196, 101);
const ORANGE: Color32 = Color32::from_rgb(239, 154, 35);
const RED: Color32 = Color32::from_rgb(225, 48, 50);
const CALIBRATION_MIN_DBA: f32 = 20.0;
const CALIBRATION_MAX_DBA: f32 = 120.0;

struct MixerState {
    devices: DeviceList,
    input_index: usize,
    controls: DspControls,
    full_scale_dba: f32,
}

impl Default for MixerState {
    fn default() -> Self {
        Self {
            devices: DeviceList::default(),
            input_index: 0,
            controls: DspControls::default(),
            full_scale_dba: 100.0,
        }
    }
}

impl MixerState {
    fn set_overdrive(&mut self, enabled: bool) {
        self.controls.overdrive = enabled;
        if !enabled {
            self.controls.gain_percent = self.controls.gain_percent.min(100.0);
        }
    }

    fn can_start(&self, status: AudioStatus) -> bool {
        status == AudioStatus::Idle && !self.devices.inputs.is_empty()
    }

    fn can_stop(status: AudioStatus) -> bool {
        matches!(status, AudioStatus::Running | AudioStatus::Fault)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AppSettings {
    input: Option<String>,
    controls: DspControls,
    full_scale_dba: f32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            input: None,
            controls: DspControls::default(),
            full_scale_dba: 100.0,
        }
    }
}

struct OpenMixerApp {
    audio: AudioEngine,
    state: MixerState,
    sample_rate: Option<u32>,
    notice: Option<String>,
    saved_input: Option<String>,
    settings_dirty: bool,
}

impl OpenMixerApp {
    fn new(context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&context.egui_ctx);
        let settings = settings_path()
            .as_deref()
            .map(load_settings)
            .unwrap_or_default();
        let mut app = Self {
            audio: AudioEngine::new(),
            state: MixerState {
                controls: settings.controls,
                full_scale_dba: settings.full_scale_dba,
                ..Default::default()
            },
            sample_rate: None,
            notice: None,
            saved_input: settings.input,
            settings_dirty: false,
        };
        app.refresh_devices();
        app.persist_settings();
        app
    }

    fn refresh_devices(&mut self) {
        match AudioEngine::devices() {
            Ok(devices) => {
                self.state.devices = devices;
                self.state.input_index =
                    preferred_input_index(&self.state.devices.inputs, self.saved_input.as_deref());
                self.saved_input =
                    remembered_input(&self.state.devices.inputs, self.saved_input.as_deref());
                self.notice = None;
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, status: AudioStatus) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("OPENMIXER")
                    .font(FontId::proportional(22.0))
                    .strong(),
            );
            ui.separator();
            small_label(ui, "LINE INPUT");
            let previous_input = self.state.input_index;
            egui::ComboBox::from_id_salt("input-device")
                .width(300.0)
                .selected_text(selected(
                    &self.state.devices.inputs,
                    self.state.input_index,
                    "No capture devices",
                ))
                .show_ui(ui, |ui| {
                    for (index, name) in self.state.devices.inputs.iter().enumerate() {
                        ui.selectable_value(&mut self.state.input_index, index, name);
                    }
                });
            if self.state.input_index != previous_input
                && let Some(name) = self
                    .state
                    .devices
                    .inputs
                    .get(self.state.input_index)
                    .cloned()
            {
                self.saved_input = Some(name);
            }
            if ui.small_button("REFRESH").clicked() {
                self.refresh_devices();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if MixerState::can_stop(status) {
                    if ui
                        .button(RichText::new("STOP").color(RED).strong())
                        .clicked()
                    {
                        self.audio.stop();
                        self.sample_rate = None;
                    }
                } else if ui
                    .add_enabled(
                        self.state.can_start(status),
                        egui::Button::new(RichText::new("LISTEN").color(GREEN).strong()),
                    )
                    .clicked()
                {
                    match self.audio.start(self.state.input_index) {
                        Ok(rate) => {
                            self.sample_rate = Some(rate);
                            self.notice = None;
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
                let (text, color) = match status {
                    AudioStatus::Running => ("LIVE", GREEN),
                    AudioStatus::Fault => ("FAULT", RED),
                    AudioStatus::Idle => ("IDLE", MUTED),
                };
                ui.label(RichText::new(text).size(10.0).strong().color(color));
            });
        });
    }

    fn cleanup(&mut self, ui: &mut egui::Ui, status: AudioStatus, t: &TelemetrySnapshot) {
        channel(ui, "CLEANUP", "01", |ui| {
            toggle(ui, &mut self.state.controls.low_cut, "LOW CUT", CYAN)
                .on_hover_text("Reduces turntable rumble below the musical bass range");
            ui.add_space(14.0);
            small_label(ui, "MAINS HUM");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.state.controls.hum_hz, None, "OFF");
                ui.selectable_value(&mut self.state.controls.hum_hz, Some(50.0), "50");
                ui.selectable_value(&mut self.state.controls.hum_hz, Some(60.0), "60");
            });
            ui.add_space(16.0);
            knob(
                ui,
                "DENOISE",
                &mut self.state.controls.noise_reduction,
                0.0..=1.0,
                |v| format!("{:.0}%", v * 100.0),
            );
            let learning = t.learning_progress < 1.0;
            if ui
                .add_enabled(
                    status == AudioStatus::Running && !learning,
                    egui::Button::new(if learning {
                        "LEARNING…"
                    } else {
                        "LEARN NOISE"
                    }),
                )
                .on_hover_text("Stop the record first. Use the lowest denoise amount that helps.")
                .clicked()
            {
                self.audio.learn_noise(2.0);
            }
            if learning {
                ui.add(egui::ProgressBar::new(t.learning_progress).fill(CYAN));
            }
            ui.add_space(10.0);
            let mut enabled = !self.state.controls.cleanup_bypassed;
            if toggle(ui, &mut enabled, "CLEAN ACTIVE", GREEN).changed() {
                self.state.controls.cleanup_bypassed = !enabled;
            }
        });
    }

    fn tone(&mut self, ui: &mut egui::Ui) {
        channel(ui, "TONE", "02", |ui| {
            knob(
                ui,
                "BASS",
                &mut self.state.controls.bass_db,
                -12.0..=12.0,
                |v| format!("{v:+.1}"),
            );
            ui.add_space(22.0);
            knob(
                ui,
                "TREBLE",
                &mut self.state.controls.treble_db,
                -12.0..=12.0,
                |v| format!("{v:+.1}"),
            );
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("dB").size(9.0).color(MUTED));
            });
        });
    }

    fn gain(&mut self, ui: &mut egui::Ui) {
        channel(ui, "LEVEL", "03", |ui| {
            let max = if self.state.controls.overdrive {
                300.0
            } else {
                100.0
            };
            ui.vertical_centered(|ui| {
                ui.add_sized(
                    [42.0, 275.0],
                    egui::Slider::new(&mut self.state.controls.gain_percent, 0.0..=max)
                        .vertical()
                        .show_value(false),
                );
                ui.label(
                    RichText::new(format!("{:.0}%", self.state.controls.gain_percent)).strong(),
                );
            });
            let mut overdrive = self.state.controls.overdrive;
            if toggle(ui, &mut overdrive, "OVERDRIVE", ORANGE).changed() {
                self.state.set_overdrive(overdrive);
            }
            toggle(ui, &mut self.state.controls.muted, "MUTE", RED);
        });
    }

    fn program_meter(&self, ui: &mut egui::Ui, t: &TelemetrySnapshot) {
        channel(ui, "PROGRAM", "04", |ui| {
            ui.horizontal(|ui| {
                signal_meter(ui, t.output_dbfs[0], "L");
                signal_meter(ui, t.output_dbfs[1], "R");
            });
            ui.add_space(10.0);
            small_label(ui, "DIGITAL HEADROOM");
            ui.label(
                RichText::new("Clipping guard active")
                    .size(10.0)
                    .color(MUTED),
            );
        });
    }

    fn loudness(&mut self, ui: &mut egui::Ui, status: AudioStatus, t: &TelemetrySnapshot) {
        let weighted = t.a_weighted_dbfs[0].max(t.a_weighted_dbfs[1]);
        let dba = displayed_dba(status, weighted, self.state.full_scale_dba);
        let color = dba
            .map(|value| band_color(dba_meter_band(value)))
            .unwrap_or(MUTED);
        channel(ui, "LOUDNESS", "MASTER", |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(dba.map_or_else(|| "--".into(), |value| format!("{value:.0}")))
                        .size(58.0)
                        .strong()
                        .color(color),
                );
                ui.label(RichText::new("dB(A)  EST.").size(12.0).strong());
            });
            ui.add_space(12.0);
            loudness_bar(ui, dba.unwrap_or(0.0));
            ui.add_space(20.0);
            small_label(ui, "CALIBRATION");
            ui.add(
                egui::Slider::new(
                    &mut self.state.full_scale_dba,
                    CALIBRATION_MIN_DBA..=CALIBRATION_MAX_DBA,
                )
                .suffix(" dBA"),
            )
            .on_hover_text("Match this reading to a sound meter at your listening position");
            ui.label(
                RichText::new("Match a sound meter at your seat. Without calibration this is only an estimate.")
                    .size(10.0)
                    .color(MUTED),
            );
            ui.add_space(14.0);
            let warning = if dba.is_none() {
                "START LISTENING TO MEASURE"
            } else if dba.unwrap_or(0.0) >= 90.0 {
                "VERY LOUD — REDUCE LEVEL"
            } else if dba.unwrap_or(0.0) >= 85.0 {
                "LIMIT LONG EXPOSURE"
            } else {
                "BELOW 85 dB(A)"
            };
            ui.label(RichText::new(warning).size(11.0).strong().color(color));
        });
    }
}

impl eframe::App for OpenMixerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let settings_before = self.current_settings();
        let ctx = root.ctx().clone();
        let status = self.audio.status();
        let telemetry = self.audio.telemetry();
        self.audio.set_controls(self.state.controls);
        if status == AudioStatus::Running {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(root, |ui| {
                self.header(ui, status);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    sized_channel(ui, 210.0, |ui| self.cleanup(ui, status, &telemetry));
                    sized_channel(ui, 150.0, |ui| self.tone(ui));
                    sized_channel(ui, 145.0, |ui| self.gain(ui));
                    sized_channel(ui, 150.0, |ui| self.program_meter(ui, &telemetry));
                    sized_channel(ui, ui.available_width().max(260.0), |ui| {
                        self.loudness(ui, status, &telemetry)
                    });
                });
                ui.add_space(8.0);
                if let Some(error) = telemetry.error.as_ref().or(self.notice.as_ref()) {
                    ui.label(RichText::new(error).color(RED));
                } else {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(
                                "LINE IN  ›  CLEANUP  ›  TONE  ›  LEVEL  ›  SYSTEM AUDIO",
                            )
                            .size(10.0)
                            .color(MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} kHz   XRUN {}   DROP {}",
                                    self.sample_rate.unwrap_or(0) / 1_000,
                                    telemetry.underruns,
                                    telemetry.overflows
                                ))
                                .size(10.0)
                                .color(MUTED),
                            );
                        });
                    });
                }
            });
        if self.current_settings() != settings_before {
            self.settings_dirty = true;
        }
        if self.settings_dirty && !ctx.input(|input| input.pointer.any_down()) {
            self.persist_settings();
            self.settings_dirty = false;
        }
    }
}

impl OpenMixerApp {
    fn current_settings(&self) -> AppSettings {
        AppSettings {
            input: self.saved_input.clone(),
            controls: self.state.controls,
            full_scale_dba: self.state.full_scale_dba,
        }
    }

    fn persist_settings(&mut self) {
        if let Some(path) = settings_path()
            && let Err(error) = save_settings(&path, &self.current_settings())
        {
            self.notice = Some(format!("Could not save settings: {error}"));
        }
    }
}

fn sized_channel(ui: &mut egui::Ui, width: f32, body: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(Vec2::new(width, 500.0), Layout::top_down(Align::Min), body);
}

fn channel(ui: &mut egui::Ui, title: &str, number: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(STRIP)
        .stroke(Stroke::new(1.0, LINE))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_min_height(470.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(11.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(number).size(9.0).color(MUTED));
                });
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(14.0);
            body(ui);
        });
}

fn small_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(9.0).strong().color(MUTED));
}

fn toggle<'a>(
    ui: &'a mut egui::Ui,
    value: &'a mut bool,
    text: &str,
    active_color: Color32,
) -> egui::Response {
    let color = if *value { active_color } else { MUTED };
    ui.toggle_value(value, RichText::new(text).size(9.0).strong().color(color))
}

fn knob(
    ui: &mut egui::Ui,
    text: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    format: impl Fn(f32) -> String,
) {
    ui.vertical_centered(|ui| {
        small_label(ui, text);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(72.0), Sense::drag());
        let min = *range.start();
        let max = *range.end();
        if response.dragged() {
            let delta = ui.input(|input| input.pointer.delta());
            *value = (*value + (delta.x - delta.y) * (max - min) / 180.0).clamp(min, max);
        }
        let center = rect.center();
        let radius = 25.0;
        ui.painter().circle_filled(center, radius + 5.0, BG);
        ui.painter().circle_filled(center, radius, RAISED);
        ui.painter()
            .circle_stroke(center, radius, Stroke::new(1.0, LINE));
        for tick in 0..11 {
            let angle = std::f32::consts::PI * (0.75 + tick as f32 * 0.15);
            ui.painter().line_segment(
                [
                    center + Vec2::angled(angle) * (radius + 8.0),
                    center + Vec2::angled(angle) * (radius + 11.0),
                ],
                Stroke::new(1.0, MUTED),
            );
        }
        let angle = std::f32::consts::PI * (0.75 + ((*value - min) / (max - min)) * 1.5);
        ui.painter().line_segment(
            [center, center + Vec2::angled(angle) * (radius - 6.0)],
            Stroke::new(3.0, TEXT),
        );
        ui.label(RichText::new(format(*value)).size(10.0));
    });
}

fn signal_meter(ui: &mut egui::Ui, dbfs: f32, channel: &str) {
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(26.0, 320.0), Sense::hover());
        for segment in 0..36 {
            let level = -60.0 + segment as f32 * (60.0 / 36.0);
            let y1 = egui::lerp(rect.bottom()..=rect.top(), segment as f32 / 36.0);
            let y2 = egui::lerp(rect.bottom()..=rect.top(), (segment + 1) as f32 / 36.0);
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left(), y2 + 1.5),
                egui::pos2(rect.right(), y1 - 1.5),
            );
            let live = if level >= -3.0 {
                RED
            } else if level >= -12.0 {
                ORANGE
            } else {
                GREEN
            };
            ui.painter().rect_filled(
                bar,
                0.0,
                if dbfs >= level {
                    live
                } else {
                    Color32::from_rgb(31, 38, 35)
                },
            );
        }
        ui.label(RichText::new(channel).size(9.0).color(MUTED));
    });
}

fn loudness_bar(ui: &mut egui::Ui, dba: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, RAISED);
    let fill = egui::lerp(rect.left()..=rect.right(), (dba / 120.0).clamp(0.0, 1.0));
    let color = band_color(dba_meter_band(dba));
    ui.painter().rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(fill, rect.bottom())),
        0.0,
        color,
    );
}

fn band_color(band: MeterBand) -> Color32 {
    match band {
        MeterBand::Green => GREEN,
        MeterBand::Orange => ORANGE,
        MeterBand::Red => RED,
    }
}

fn displayed_dba(status: AudioStatus, weighted_dbfs: f32, calibration: f32) -> Option<f32> {
    (status == AudioStatus::Running).then(|| estimated_dba(weighted_dbfs, calibration))
}

fn selected<'a>(items: &'a [String], index: usize, empty: &'static str) -> &'a str {
    items.get(index).map(String::as_str).unwrap_or(empty)
}

fn preferred_input_index(inputs: &[String], saved: Option<&str>) -> usize {
    saved
        .and_then(|name| inputs.iter().position(|input| input == name))
        .unwrap_or(0)
}

fn remembered_input(inputs: &[String], saved: Option<&str>) -> Option<String> {
    saved.map(str::to_owned).or_else(|| inputs.first().cloned())
}

fn settings_path() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".openmixer").join("settings.ini"))
}

fn load_settings(path: &std::path::Path) -> AppSettings {
    let mut settings = AppSettings::default();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return settings;
    };
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "input" if !value.is_empty() => settings.input = Some(value.into()),
            "hum_hz" => {
                settings.controls.hum_hz = match value {
                    "50" => Some(50.0),
                    "60" => Some(60.0),
                    _ => None,
                }
            }
            "low_cut" => parse_bool(value, &mut settings.controls.low_cut),
            "noise_reduction" => parse_f32(value, &mut settings.controls.noise_reduction),
            "bass_db" => parse_f32(value, &mut settings.controls.bass_db),
            "treble_db" => parse_f32(value, &mut settings.controls.treble_db),
            "gain_percent" => parse_f32(value, &mut settings.controls.gain_percent),
            "overdrive" => parse_bool(value, &mut settings.controls.overdrive),
            "muted" => parse_bool(value, &mut settings.controls.muted),
            "cleanup_bypassed" => parse_bool(value, &mut settings.controls.cleanup_bypassed),
            "calibration_dba" => parse_f32(value, &mut settings.full_scale_dba),
            _ => {}
        }
    }
    settings.controls.noise_reduction = settings.controls.noise_reduction.clamp(0.0, 1.0);
    settings.controls.bass_db = settings.controls.bass_db.clamp(-12.0, 12.0);
    settings.controls.treble_db = settings.controls.treble_db.clamp(-12.0, 12.0);
    let gain_max = if settings.controls.overdrive {
        300.0
    } else {
        100.0
    };
    settings.controls.gain_percent = settings.controls.gain_percent.clamp(0.0, gain_max);
    settings.full_scale_dba = settings
        .full_scale_dba
        .clamp(CALIBRATION_MIN_DBA, CALIBRATION_MAX_DBA);
    settings
}

fn parse_bool(value: &str, target: &mut bool) {
    if let Ok(parsed) = value.parse() {
        *target = parsed;
    }
}

fn parse_f32(value: &str, target: &mut f32) {
    if let Ok(parsed) = value.parse() {
        *target = parsed;
    }
}

fn save_settings(path: &std::path::Path, settings: &AppSettings) -> std::io::Result<()> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let hum = settings.controls.hum_hz.unwrap_or(0.0);
    let input = settings
        .input
        .as_deref()
        .unwrap_or("")
        .replace(['\r', '\n'], " ");
    std::fs::write(
        path,
        format!(
            "[openmixer]\ninput={input}\nhum_hz={hum:.0}\nlow_cut={}\nnoise_reduction={}\nbass_db={}\ntreble_db={}\ngain_percent={}\noverdrive={}\nmuted={}\ncleanup_bypassed={}\ncalibration_dba={}\n",
            settings.controls.low_cut,
            settings.controls.noise_reduction,
            settings.controls.bass_db,
            settings.controls.treble_db,
            settings.controls.gain_percent,
            settings.controls.overdrive,
            settings.controls.muted,
            settings.controls.cleanup_bypassed,
            settings.full_scale_dba,
        ),
    )
}

fn configure_style(context: &egui::Context) {
    let mut style = (*context.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(7.0, 7.0);
    style.spacing.button_padding = Vec2::new(9.0, 6.0);
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = STRIP;
    style.visuals.widgets.inactive.bg_fill = RAISED;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(36, 40, 41);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(37, 64, 61);
    style.visuals.selection.bg_fill = Color32::from_rgb(31, 91, 84);
    style.visuals.override_text_color = Some(TEXT);
    context.set_global_style(style);
}

fn main() -> eframe::Result {
    eframe::run_native(
        "OpenMixer",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1160.0, 620.0])
                .with_min_inner_size([980.0, 600.0]),
            ..Default::default()
        },
        Box::new(|context| Ok(Box::new(OpenMixerApp::new(context)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_gain_is_conservative() {
        assert_eq!(MixerState::default().controls.gain_percent, 25.0);
    }

    #[test]
    fn disabling_overdrive_clamps_gain_to_one_hundred() {
        let mut state = MixerState::default();
        state.controls.overdrive = true;
        state.controls.gain_percent = 260.0;
        state.set_overdrive(false);
        assert_eq!(state.controls.gain_percent, 100.0);
    }

    #[test]
    fn start_requires_a_selected_input_and_idle_engine() {
        let mut state = MixerState::default();
        assert!(!state.can_start(AudioStatus::Idle));
        state.devices.inputs.push("Line In".into());
        assert!(state.can_start(AudioStatus::Idle));
        assert!(!state.can_start(AudioStatus::Running));
    }

    #[test]
    fn backend_fault_still_exposes_stop() {
        assert!(MixerState::can_stop(AudioStatus::Fault));
        assert!(MixerState::can_stop(AudioStatus::Running));
        assert!(!MixerState::can_stop(AudioStatus::Idle));
    }

    #[test]
    fn loudness_is_only_reported_for_a_running_stream() {
        assert_eq!(displayed_dba(AudioStatus::Idle, -20.0, 100.0), None);
        assert_eq!(
            displayed_dba(AudioStatus::Running, -20.0, 100.0),
            Some(80.0)
        );
    }

    #[test]
    fn saved_input_is_restored_by_device_name() {
        let inputs = vec!["Microphone".into(), "Line In".into()];
        assert_eq!(preferred_input_index(&inputs, Some("Line In")), 1);
        assert_eq!(preferred_input_index(&inputs, Some("Missing")), 0);
        assert_eq!(
            remembered_input(&inputs, Some("Disconnected Line In")).as_deref(),
            Some("Disconnected Line In")
        );
        assert_eq!(
            remembered_input(&inputs, None).as_deref(),
            Some("Microphone")
        );
    }

    #[test]
    fn all_controls_round_trip_through_ini_settings() {
        let directory =
            std::env::temp_dir().join(format!("openmixer-settings-test-{}", std::process::id()));
        let path = directory.join("settings.ini");
        let settings = AppSettings {
            input: Some("Line In (USB Audio)".into()),
            controls: DspControls {
                hum_hz: Some(60.0),
                low_cut: true,
                noise_reduction: 0.35,
                bass_db: 2.5,
                treble_db: -1.5,
                gain_percent: 72.0,
                overdrive: true,
                muted: true,
                cleanup_bypassed: true,
            },
            full_scale_dba: 42.0,
        };
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path), settings);
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("ini")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn calibration_loaded_below_twenty_is_clamped() {
        let directory =
            std::env::temp_dir().join(format!("openmixer-calibration-test-{}", std::process::id()));
        let path = directory.join("settings.ini");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, "[openmixer]\ncalibration_dba=5\n").unwrap();
        assert_eq!(load_settings(&path).full_scale_dba, 20.0);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
