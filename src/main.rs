use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Stroke, Vec2};
use openmixer::{
    audio::{AudioEngine, AudioStatus, DeviceList, TelemetrySnapshot},
    dsp::{DspControls, MeterBand, meter_band},
};

const BACKGROUND: Color32 = Color32::from_rgb(20, 23, 24);
const PANEL: Color32 = Color32::from_rgb(29, 33, 34);
const PANEL_RAISED: Color32 = Color32::from_rgb(37, 42, 43);
const CREAM: Color32 = Color32::from_rgb(232, 226, 207);
const MUTED: Color32 = Color32::from_rgb(145, 153, 150);
const TEAL: Color32 = Color32::from_rgb(54, 190, 169);
const GREEN: Color32 = Color32::from_rgb(69, 196, 111);
const ORANGE: Color32 = Color32::from_rgb(238, 158, 57);
const RED: Color32 = Color32::from_rgb(232, 75, 69);

struct MixerState {
    devices: DeviceList,
    input_index: usize,
    output_index: usize,
    controls: DspControls,
}

impl Default for MixerState {
    fn default() -> Self {
        Self { devices: DeviceList::default(), input_index: 0, output_index: 0, controls: DspControls::default() }
    }
}

impl MixerState {
    fn set_overdrive(&mut self, enabled: bool) {
        self.controls.overdrive = enabled;
        if !enabled { self.controls.gain_percent = self.controls.gain_percent.min(100.0); }
    }

    fn can_start(&self, status: AudioStatus) -> bool {
        status == AudioStatus::Idle && !self.devices.inputs.is_empty() && !self.devices.outputs.is_empty()
    }
}

fn meter_color(level_dbfs: f32) -> Color32 {
    match meter_band(level_dbfs) { MeterBand::Green => GREEN, MeterBand::Orange => ORANGE, MeterBand::Red => RED }
}

struct OpenMixerApp {
    audio: AudioEngine,
    state: MixerState,
    sample_rate: Option<u32>,
    notice: Option<String>,
}

impl OpenMixerApp {
    fn new(context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&context.egui_ctx);
        let mut app = Self { audio: AudioEngine::new(), state: MixerState::default(), sample_rate: None, notice: None };
        app.refresh_devices();
        app
    }

    fn refresh_devices(&mut self) {
        match AudioEngine::devices() {
            Ok(devices) => {
                self.state.devices = devices;
                self.state.input_index = self.state.input_index.min(self.state.devices.inputs.len().saturating_sub(1));
                self.state.output_index = self.state.output_index.min(self.state.devices.outputs.len().saturating_sub(1));
                self.notice = None;
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn routing(&mut self, ui: &mut egui::Ui, status: AudioStatus) {
        ui.horizontal(|ui| {
            ui.label(section_label("SOURCE"));
            ui.add_space(12.0);
            ui.label(RichText::new("Listen to").color(MUTED));
            egui::ComboBox::from_id_salt("input-device").width(270.0)
                .selected_text(selected(&self.state.devices.inputs, self.state.input_index, "No capture devices"))
                .show_ui(ui, |ui| for (index, name) in self.state.devices.inputs.iter().enumerate() { ui.selectable_value(&mut self.state.input_index, index, name); });
            ui.label(RichText::new("Play through").color(MUTED));
            egui::ComboBox::from_id_salt("output-device").width(270.0)
                .selected_text(selected(&self.state.devices.outputs, self.state.output_index, "No playback devices"))
                .show_ui(ui, |ui| for (index, name) in self.state.devices.outputs.iter().enumerate() { ui.selectable_value(&mut self.state.output_index, index, name); });
            if ui.button("↻ Refresh").on_hover_text("Scan the operating system for audio devices again").clicked() { self.refresh_devices(); }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match status {
                    AudioStatus::Running => if ui.add(egui::Button::new(RichText::new("■  STOP").color(RED))).clicked() { self.audio.stop(); self.sample_rate = None; },
                    _ => if ui.add_enabled(self.state.can_start(status), egui::Button::new(RichText::new("▶  START").color(TEAL))).clicked() {
                        match self.audio.start(self.state.input_index, self.state.output_index) {
                            Ok(rate) => { self.sample_rate = Some(rate); self.notice = None; }
                            Err(error) => self.notice = Some(error),
                        }
                    },
                }
            });
        });
    }

    fn cleanup(&mut self, ui: &mut egui::Ui, status: AudioStatus, telemetry: &TelemetrySnapshot) {
        panel(ui, "NOISE CLEANUP", |ui| {
            ui.checkbox(&mut self.state.controls.low_cut, "Low cut · 28 Hz").on_hover_text("Reduces sub-bass rumble without removing ordinary bass notes");
            ui.add_space(10.0);
            ui.label(control_label("MAINS HUM"));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.state.controls.hum_hz, None, "Off");
                ui.selectable_value(&mut self.state.controls.hum_hz, Some(50.0), "50 Hz");
                ui.selectable_value(&mut self.state.controls.hum_hz, Some(60.0), "60 Hz");
            });
            ui.add_space(14.0);
            ui.label(control_label("LEARNED REDUCTION"));
            ui.add(egui::Slider::new(&mut self.state.controls.noise_reduction, 0.0..=1.0).show_value(false));
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}%", (self.state.controls.noise_reduction * 100.0).round())).color(CREAM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let learning = telemetry.learning_progress < 1.0;
                    if ui.add_enabled(status == AudioStatus::Running && !learning, egui::Button::new(if learning { "Learning…" } else { "Learn idle noise" }))
                        .on_hover_text("Stop the turntable and capture two seconds of steady electrical noise").clicked() { self.audio.learn_noise(2.0); }
                });
            });
            if telemetry.learning_progress < 1.0 { ui.add(egui::ProgressBar::new(telemetry.learning_progress).fill(TEAL)); }
            ui.add_space(12.0);
            ui.checkbox(&mut self.state.controls.cleanup_bypassed, "Bypass cleanup").on_hover_text("Compare filters at the same master level");
        });
    }

    fn tone(&mut self, ui: &mut egui::Ui) {
        panel(ui, "TONE", |ui| {
            tone_control(ui, "BASS", &mut self.state.controls.bass_db);
            ui.add_space(24.0);
            tone_control(ui, "TREBLE", &mut self.state.controls.treble_db);
            ui.add_space(24.0);
            ui.label(RichText::new("Designed for music, not speech").color(MUTED).small());
            ui.label(RichText::new("Strong cleanup may soften quiet detail.").color(MUTED).small());
        });
    }

    fn master(&mut self, ui: &mut egui::Ui, telemetry: &TelemetrySnapshot) {
        panel(ui, "MASTER OUTPUT", |ui| {
            ui.horizontal(|ui| {
                stereo_meter(ui, "INPUT", telemetry.input_dbfs);
                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    let max = if self.state.controls.overdrive { 300.0 } else { 100.0 };
                    ui.label(control_label("LEVEL"));
                    ui.add_sized([42.0, 245.0], egui::Slider::new(&mut self.state.controls.gain_percent, 0.0..=max).vertical().show_value(false));
                    ui.label(RichText::new(format!("{:.0}%", self.state.controls.gain_percent)).color(CREAM).strong());
                });
                ui.add_space(18.0);
                stereo_meter(ui, "OUTPUT", telemetry.output_dbfs);
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let mut overdrive = self.state.controls.overdrive;
                let overdrive_color = if overdrive { ORANGE } else { MUTED };
                if ui.toggle_value(&mut overdrive, RichText::new("OVERDRIVE").color(overdrive_color))
                    .on_hover_text("Extends master gain to 300%. Peak protection remains active; use speakers carefully.").changed() { self.state.set_overdrive(overdrive); }
                let mute_color = if self.state.controls.muted { RED } else { MUTED };
                ui.toggle_value(&mut self.state.controls.muted, RichText::new("MUTE").color(mute_color));
            });
            ui.add_space(8.0);
            ui.label(RichText::new("dBFS signal headroom · not ear loudness").color(MUTED).small())
                .on_hover_text("These meters cannot measure dB SPL at your ears. Speaker gain, amplifier volume and distance are unknown.");
        });
    }
}

impl eframe::App for OpenMixerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        let status = self.audio.status();
        let telemetry = self.audio.telemetry();
        self.audio.set_controls(self.state.controls);
        if status == AudioStatus::Running { context.request_repaint_after(std::time::Duration::from_millis(33)); }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::same(20))).show(root, |ui| {
            egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::symmetric(4, 8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("OPENMIXER").font(FontId::proportional(27.0)).strong().color(CREAM));
                        ui.label(RichText::new("VINYL INPUT CONSOLE").color(MUTED).size(11.0));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (text, color) = match status { AudioStatus::Running => (format!("●  MONITORING  ·  {} kHz", self.sample_rate.unwrap_or(0) / 1_000), GREEN), AudioStatus::Fault => ("●  AUDIO FAULT".into(), RED), AudioStatus::Idle => ("●  IDLE".into(), MUTED) };
                        ui.label(RichText::new(text).color(color).strong());
                    });
                });
            });
            ui.add_space(10.0);
            egui::Frame::new().fill(PANEL).corner_radius(8).inner_margin(egui::Margin::same(14)).show(ui, |ui| self.routing(ui, status));
            ui.add_space(14.0);
            ui.columns(3, |columns| {
                self.cleanup(&mut columns[0], status, &telemetry);
                self.tone(&mut columns[1]);
                self.master(&mut columns[2], &telemetry);
            });
            ui.add_space(12.0);
            if let Some(error) = telemetry.error.as_ref().or(self.notice.as_ref()) {
                egui::Frame::new().fill(Color32::from_rgb(62, 31, 30)).corner_radius(6).inner_margin(egui::Margin::same(10)).show(ui, |ui| { ui.label(RichText::new(error).color(Color32::from_rgb(255, 180, 170))); });
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SIGNAL FLOW").color(MUTED).small());
                    ui.label(RichText::new("LINE IN  →  HUM  →  DENOISE  →  TONE  →  MASTER  →  SELECTED OUTPUT").color(CREAM).small());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| { ui.label(RichText::new(format!("XRUN  {}  ·  DROP  {}", telemetry.underruns, telemetry.overflows)).color(MUTED).small()); });
                });
            }
        });
    }
}

fn selected<'a>(items: &'a [String], index: usize, empty: &'static str) -> &'a str { items.get(index).map(String::as_str).unwrap_or(empty) }
fn section_label(text: &str) -> RichText { RichText::new(text).color(TEAL).strong().size(11.0) }
fn control_label(text: &str) -> RichText { RichText::new(text).color(MUTED).strong().size(10.0) }

fn panel(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, PANEL_RAISED)).corner_radius(8).inner_margin(egui::Margin::same(18)).show(ui, |ui| {
        ui.set_min_height(385.0);
        ui.label(section_label(title));
        ui.add_space(18.0);
        body(ui);
    });
}

fn tone_control(ui: &mut egui::Ui, label: &str, value: &mut f32) {
    ui.horizontal(|ui| {
        ui.label(control_label(label));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| { ui.label(RichText::new(format!("{:+.1} dB", *value)).color(CREAM)); });
    });
    ui.add(egui::Slider::new(value, -12.0..=12.0).show_value(false));
}

fn stereo_meter(ui: &mut egui::Ui, title: &str, levels: [f32; 2]) {
    ui.vertical_centered(|ui| {
        ui.label(control_label(title));
        ui.horizontal(|ui| { level_meter(ui, levels[0], "L"); level_meter(ui, levels[1], "R"); });
        ui.label(RichText::new(format!("{:.1} dB", levels[0].max(levels[1]))).color(meter_color(levels[0].max(levels[1]))).monospace().size(11.0));
    });
}

fn level_meter(ui: &mut egui::Ui, level: f32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(18.0, 245.0), egui::Sense::hover());
    let segments = 30;
    for segment in 0..segments {
        let low = -60.0 + segment as f32 * 2.0;
        let y1 = egui::lerp(rect.bottom()..=rect.top(), segment as f32 / segments as f32);
        let y2 = egui::lerp(rect.bottom()..=rect.top(), (segment + 1) as f32 / segments as f32);
        let segment_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), y2 + 1.0), egui::pos2(rect.right(), y1 - 1.0));
        let color = if level >= low { meter_color(low) } else { Color32::from_rgb(48, 53, 53) };
        ui.painter().rect_filled(segment_rect, 1.0, color);
    }
    ui.put(egui::Rect::from_min_size(egui::pos2(rect.left(), rect.bottom() + 2.0), Vec2::new(18.0, 16.0)), egui::Label::new(RichText::new(label).color(MUTED).size(9.0)));
}

fn configure_style(context: &egui::Context) {
    let mut style = (*context.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(9.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = PANEL;
    style.visuals.widgets.inactive.bg_fill = PANEL_RAISED;
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, CREAM);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 57, 57);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(35, 97, 88);
    style.visuals.selection.bg_fill = Color32::from_rgb(31, 111, 99);
    style.visuals.override_text_color = Some(CREAM);
    context.set_global_style(style);
}

fn main() -> eframe::Result {
    eframe::run_native(
        "OpenMixer",
        eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([1120.0, 640.0]).with_min_inner_size([900.0, 590.0]), ..Default::default() },
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
    fn start_requires_both_selected_devices_and_idle_engine() {
        let mut state = MixerState::default();
        assert!(!state.can_start(AudioStatus::Idle));
        state.devices.inputs.push("Line In".into());
        state.devices.outputs.push("Speakers".into());
        assert!(state.can_start(AudioStatus::Idle));
        assert!(!state.can_start(AudioStatus::Running));
    }

    #[test]
    fn meter_colors_follow_dbfs_thresholds() {
        assert_eq!(meter_color(-18.0), GREEN);
        assert_eq!(meter_color(-12.0), ORANGE);
        assert_eq!(meter_color(-3.0), RED);
    }
}
