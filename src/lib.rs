mod core;
mod registry;
mod shmem;
mod ui_state;

use std::sync::Arc;

use truce::prelude::*;
use truce_egui::{EguiEditor, ParamState};

use crate::core::{
    cqt_center_frequencies, db_to_y, format_freq, freq_to_x, x_to_freq, AnalyzerCore,
    SpectrumData, DB_FLOOR, DB_GRID, FREQ_GRID, FREQ_MAX, FREQ_MIN, MODE_BOTH, MODE_DIFF,
    MODE_LEFT, MODE_RIGHT, MODE_SUM,
};
use crate::registry::InstanceId;
use crate::shmem::{FileRegistry, SharedMemoryWriter};
use crate::ui_state::{PersistentState, UiState, ViewMode};

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(ParamEnum)]
pub enum ChannelMode {
    Sum,
    Both,
    Left,
    Right,
    Diff,
}

impl ChannelMode {
    fn as_mode_u8(self) -> u8 {
        match self {
            Self::Left => MODE_LEFT,
            Self::Right => MODE_RIGHT,
            Self::Sum => MODE_SUM,
            Self::Diff => MODE_DIFF,
            Self::Both => MODE_BOTH,
        }
    }
}

#[derive(Params)]
pub struct TruceAnalyzerParams {
    #[param(name = "Gain", range = "linear(-60, 6)", unit = "dB", smooth = "exp(5)")]
    pub gain: FloatParam,

    #[param(name = "Channel")]
    pub channel: EnumParam<ChannelMode>,
}

use TruceAnalyzerParamsParamId as P;

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct TruceAnalyzer {
    params: Arc<TruceAnalyzerParams>,
    core: AnalyzerCore,
    instance_id: InstanceId,
    state: PersistentState,
}

impl TruceAnalyzer {
    pub fn new(params: Arc<TruceAnalyzerParams>) -> Self {
        let freqs = cqt_center_frequencies();
        let spectrum = Arc::new(SpectrumData::new(freqs));

        let instance_id = registry::register(None, spectrum.clone());
        let instance_name = registry::name_of(instance_id).unwrap_or_default();

        let mut core = AnalyzerCore::new(spectrum);
        if let Some(writer) =
            SharedMemoryWriter::create(instance_id.0, &instance_name, core.spectrum().num_bins())
        {
            core.set_shmem_writer(writer);
        }

        let mut file_reg = FileRegistry::load();
        file_reg.add(instance_id.0, &instance_name);

        Self {
            params,
            core,
            instance_id,
            state: PersistentState {
                instance_name,
                ..Default::default()
            },
        }
    }
}

impl Drop for TruceAnalyzer {
    fn drop(&mut self) {
        registry::deregister(self.instance_id);
        let mut file_reg = FileRegistry::load();
        file_reg.remove(self.instance_id.0);
    }
}

impl PluginLogic for TruceAnalyzer {
    fn reset(&mut self, sr: f64, _bs: usize) {
        self.params.set_sample_rate(sr);
        self.core.reset(sr);
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let channels = buffer.channels();
        if channels == 0 {
            return ProcessStatus::Normal;
        }
        let mode = if self.core.spectrum().has_remotes() {
            MODE_SUM
        } else {
            self.params.channel.value().as_mode_u8()
        };
        self.core.spectrum().set_mode(mode);

        for i in 0..buffer.num_samples() {
            let gain = db_to_linear(self.params.gain.smoothed_next() as f64) as f32;

            let mut left = 0.0f32;
            let mut right = 0.0f32;
            for ch in 0..channels {
                let (inp, out) = buffer.io(ch);
                out[i] = inp[i] * gain;
                match ch {
                    0 => left = out[i],
                    1 => right = out[i],
                    _ => {}
                }
            }
            if channels < 2 {
                right = left;
            }

            self.core.process_stereo(left, right);
        }
        ProcessStatus::Normal
    }

    fn save_state(&self) -> Vec<u8> {
        self.state.serialize()
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Some(ps) = PersistentState::deserialize(data) {
            if !ps.instance_name.is_empty() {
                registry::rename(self.instance_id, &ps.instance_name);
            }
            self.state = ps;
        }
    }

    fn custom_editor(&self) -> Option<Box<dyn truce_core::editor::Editor>> {
        let spectrum = self.core.spectrum().clone();
        let instance_id = self.instance_id;

        Some(Box::new(
            EguiEditor::with_ui(
                (800, 400),
                AnalyzerEditorUi {
                    ui: UiState::new(spectrum, instance_id),
                },
            )
            .with_visuals(truce_egui::theme::dark())
            .with_font(truce_gui::font::JETBRAINS_MONO),
        ))
    }
}

// ---------------------------------------------------------------------------
// EditorUi impl — bridges UiState with truce's state lifecycle
// ---------------------------------------------------------------------------

struct AnalyzerEditorUi {
    ui: UiState,
}

impl truce_egui::EditorUi for AnalyzerEditorUi {
    fn ui(&mut self, ctx: &egui::Context, state: &ParamState) {
        self.ui.update_local();
        self.ui.update_remotes();
        self.ui.update_diff();
        analyzer_ui(ctx, state, &mut self.ui);
    }

    fn opened(&mut self, state: &ParamState) {
        self.ui.apply_state(state);
    }

    fn state_changed(&mut self, state: &ParamState) {
        self.ui.apply_state(state);
    }
}

truce::plugin! {
    logic: TruceAnalyzer,
    params: TruceAnalyzerParams,
    bus_layouts: [BusLayout::stereo()],
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

const LEFT_MARGIN: f32 = 45.0;
const TOP_MARGIN: f32 = 10.0;
const RIGHT_MARGIN: f32 = 10.0;
const BOTTOM_MARGIN: f32 = 20.0;

use truce_egui::theme;

const GRID_COLOR: egui::Color32 = egui::Color32::from_rgb(42, 42, 74);
const STROKE_L: egui::Color32 = egui::Color32::from_rgb(106, 176, 255);
const FILL_L: egui::Color32 = egui::Color32::from_rgba_premultiplied(30, 58, 87, 50);
const STROKE_R: egui::Color32 = egui::Color32::from_rgb(255, 140, 90);
const FILL_R: egui::Color32 = egui::Color32::from_rgba_premultiplied(87, 45, 25, 50);
#[allow(dead_code)]
const STROKE_GHOST: egui::Color32 = egui::Color32::from_rgba_premultiplied(180, 180, 180, 120);
#[allow(dead_code)]
const FILL_GHOST: egui::Color32 = egui::Color32::from_rgba_premultiplied(100, 100, 100, 25);
const STROKE_DIFF_POS: egui::Color32 = egui::Color32::from_rgb(230, 80, 80);
const FILL_DIFF_POS: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 30, 30, 50);
const STROKE_DIFF_NEG: egui::Color32 = egui::Color32::from_rgb(80, 200, 120);
const FILL_DIFF_NEG: egui::Color32 = egui::Color32::from_rgba_premultiplied(30, 80, 45, 50);

const GHOST_PALETTE: &[(egui::Color32, egui::Color32)] = &[
    (
        egui::Color32::from_rgba_premultiplied(180, 180, 180, 120),
        egui::Color32::from_rgba_premultiplied(100, 100, 100, 25),
    ),
    (
        egui::Color32::from_rgba_premultiplied(255, 200, 100, 120),
        egui::Color32::from_rgba_premultiplied(100, 80, 30, 25),
    ),
    (
        egui::Color32::from_rgba_premultiplied(150, 255, 150, 120),
        egui::Color32::from_rgba_premultiplied(40, 90, 40, 25),
    ),
    (
        egui::Color32::from_rgba_premultiplied(200, 150, 255, 120),
        egui::Color32::from_rgba_premultiplied(70, 50, 100, 25),
    ),
];

fn analyzer_ui(ctx: &egui::Context, state: &ParamState, ui_state: &mut UiState) {
    draw_header(ctx, state, ui_state);
    draw_central(ctx, ui_state);
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn draw_header(ctx: &egui::Context, state: &ParamState, ui_state: &mut UiState) {
    egui::TopBottomPanel::top("header")
        .exact_height(30.0)
        .frame(egui::Frame::NONE.fill(theme::HEADER_BG))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(10.0);

                // Fixed title + editable instance name
                ui.label(
                    egui::RichText::new("Truce Analyzer:")
                        .size(14.0)
                        .color(theme::HEADER_TEXT)
                        .strong(),
                );
                if ui_state.editing_name {
                    let resp = ui.text_edit_singleline(&mut ui_state.instance_name);
                    if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ui_state.editing_name = false;
                        registry::rename(ui_state.instance_id, &ui_state.instance_name);
                        ui_state.sync_to_plugin(state);
                    }
                } else {
                    let label = ui.label(
                        egui::RichText::new(&ui_state.instance_name)
                            .size(14.0)
                            .color(theme::PRIMARY)
                            .strong(),
                    );
                    if label.double_clicked() {
                        ui_state.editing_name = true;
                    }
                }

                // Right-aligned controls
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);

                    let dim = |s| egui::RichText::new(s).size(10.0).color(theme::TEXT_DIM);

                    // Channel (hidden when comparison sources are selected)
                    let channel_id: u32 = P::Channel.into();
                    if ui_state.selected_ids.is_empty() {
                        let current_ch = state.format(channel_id);
                        let ch_options = ["Sum", "Both", "Left", "Right", "Diff"];
                        egui::ComboBox::from_id_salt("channel_mode")
                            .selected_text(current_ch)
                            .width(55.0)
                            .show_ui(ui, |ui| {
                                for (i, &label) in ch_options.iter().enumerate() {
                                    let norm = i as f64 / (ch_options.len() - 1) as f64;
                                    let sel = (state.get(channel_id) - norm).abs() < 0.01;
                                    if ui.selectable_label(sel, label).clicked() {
                                        state.set_immediate(channel_id, norm);
                                    }
                                }
                            });
                        ui.label(dim("Channel"));
                    }

                    // View (hidden when no remotes selected)
                    if !ui_state.selected_ids.is_empty() {
                        ui.add_space(4.0);
                        let view_label = match ui_state.view_mode {
                            ViewMode::Both => "Both",
                            ViewMode::Normal => "Normal",
                            ViewMode::Diff => "Diff",
                        };
                        egui::ComboBox::from_id_salt("view_mode")
                            .selected_text(view_label)
                            .width(60.0)
                            .show_ui(ui, |ui| {
                                for &(mode, label) in &[
                                    (ViewMode::Both, "Both"),
                                    (ViewMode::Normal, "Normal"),
                                    (ViewMode::Diff, "Diff"),
                                ] {
                                    if ui
                                        .selectable_label(ui_state.view_mode == mode, label)
                                        .clicked()
                                    {
                                        ui_state.set_view_mode(mode, state);
                                    }
                                }
                            });
                        ui.label(dim("View"));
                    }

                    ui.add_space(4.0);

                    // Source
                    let current_pid = std::process::id();
                    let mut all_instances = registry::list();
                    let file_reg = FileRegistry::load();
                    for entry in &file_reg.instances {
                        if entry.pid == current_pid {
                            continue;
                        }
                        let id = InstanceId(entry.id);
                        if !all_instances.iter().any(|(i, _)| *i == id) {
                            all_instances.push((id, entry.name.clone()));
                        }
                    }
                    let other_instances: Vec<_> = all_instances
                        .iter()
                        .filter(|(id, _)| *id != ui_state.instance_id)
                        .collect();

                    let source_label = if ui_state.selected_ids.is_empty() {
                        "Self".to_string()
                    } else if ui_state.selected_ids.len() == 1 {
                        registry::name_of(ui_state.selected_ids[0])
                            .unwrap_or_else(|| "?".to_string())
                    } else {
                        format!("{} sources", ui_state.selected_ids.len())
                    };

                    egui::ComboBox::from_id_salt("source_select")
                        .selected_text(&source_label)
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            if other_instances.is_empty() {
                                ui.label(
                                    egui::RichText::new("No other instances")
                                        .color(theme::TEXT_DIM),
                                );
                            }
                            for (id, name) in &other_instances {
                                let checked = ui_state.selected_ids.contains(id);
                                if ui.selectable_label(checked, name).clicked() {
                                    ui_state.toggle_source(*id, state);
                                }
                            }
                        });
                    ui.label(dim("Source"));
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Central panel
// ---------------------------------------------------------------------------

fn draw_central(ctx: &egui::Context, ui_state: &UiState) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(theme::BACKGROUND))
        .show(ctx, |ui| {
            let (response, painter) =
                ui.allocate_painter(ui.available_size(), egui::Sense::hover());
            let rect = response.rect;

            let spec = egui::Rect::from_min_max(
                egui::pos2(rect.left() + LEFT_MARGIN, rect.top() + TOP_MARGIN),
                egui::pos2(rect.right() - RIGHT_MARGIN, rect.bottom() - BOTTOM_MARGIN),
            );

            let center_freqs = ui_state.spectrum.center_freqs_slice();

            draw_grid(&painter, spec);

            match ui_state.view_mode {
                ViewMode::Normal => {
                    for (idx, remote) in ui_state.remotes.iter().enumerate() {
                        let (stroke, fill) = GHOST_PALETTE[idx % GHOST_PALETTE.len()];
                        draw_spectrum(&painter, center_freqs, &remote.bins, spec, stroke, fill);
                    }
                    if ui_state.spectrum.is_both() {
                        draw_spectrum(
                            &painter,
                            center_freqs,
                            &ui_state.bins_b,
                            spec,
                            STROKE_R,
                            FILL_R,
                        );
                    }
                    draw_spectrum(
                        &painter,
                        center_freqs,
                        &ui_state.bins_a,
                        spec,
                        STROKE_L,
                        FILL_L,
                    );
                }
                ViewMode::Diff => {
                    if !ui_state.remotes.is_empty() {
                        for remote in &ui_state.remotes {
                            draw_diff_spectrum(
                                &painter,
                                center_freqs,
                                &remote.diff_bins,
                                spec,
                            );
                        }
                    } else {
                        draw_spectrum(
                            &painter,
                            center_freqs,
                            &ui_state.bins_a,
                            spec,
                            STROKE_L,
                            FILL_L,
                        );
                    }
                }
                ViewMode::Both => {
                    for (idx, remote) in ui_state.remotes.iter().enumerate() {
                        let (stroke, fill) = GHOST_PALETTE[idx % GHOST_PALETTE.len()];
                        draw_spectrum(&painter, center_freqs, &remote.bins, spec, stroke, fill);
                    }
                    draw_spectrum(
                        &painter,
                        center_freqs,
                        &ui_state.bins_a,
                        spec,
                        STROKE_L,
                        FILL_L,
                    );
                    for remote in &ui_state.remotes {
                        draw_diff_spectrum(
                            &painter,
                            center_freqs,
                            &remote.diff_bins,
                            spec,
                        );
                    }
                }
            }

            draw_labels(&painter, spec);
            draw_legend(&painter, ui_state, spec);

            if let Some(pos) = response.hover_pos() {
                if spec.contains(pos) {
                    draw_hover(&painter, pos, ui_state, spec);
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

fn draw_grid(painter: &egui::Painter, rect: egui::Rect) {
    let stroke = egui::Stroke::new(0.5, GRID_COLOR);

    for &db in DB_GRID {
        let y = db_to_y(db, rect.top(), rect.height());
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            stroke,
        );
    }

    for &freq in FREQ_GRID {
        let x = freq_to_x(freq, rect.left(), rect.width());
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            stroke,
        );
    }
}

// ---------------------------------------------------------------------------
// Spectrum curve (filled mesh + stroke)
// ---------------------------------------------------------------------------

fn draw_spectrum(
    painter: &egui::Painter,
    center_freqs: &[f32],
    bins: &[f32],
    rect: egui::Rect,
    stroke_color: egui::Color32,
    fill_color: egui::Color32,
) {
    let mut curve_points: Vec<egui::Pos2> = Vec::with_capacity(bins.len());

    let mut last_px = -1i32;
    for (i, &db) in bins.iter().enumerate().take(center_freqs.len()) {
        let freq = center_freqs[i];
        if freq < FREQ_MIN || freq > FREQ_MAX {
            continue;
        }
        let x = freq_to_x(freq, rect.left(), rect.width());
        let px = x as i32;
        if px == last_px {
            continue;
        }
        last_px = px;
        let y = db_to_y(db, rect.top(), rect.height());
        curve_points.push(egui::pos2(x, y));
    }

    if curve_points.len() < 2 {
        return;
    }

    let mut mesh = egui::Mesh::default();
    for (idx, &point) in curve_points.iter().enumerate() {
        mesh.colored_vertex(point, fill_color);
        mesh.colored_vertex(egui::pos2(point.x, rect.bottom()), fill_color);
        if idx > 0 {
            let i = (idx * 2) as u32;
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i + 1, i);
        }
    }
    painter.add(egui::Shape::mesh(mesh));

    painter.add(egui::Shape::line(
        curve_points,
        egui::Stroke::new(1.5, stroke_color),
    ));
}

// ---------------------------------------------------------------------------
// Diff spectrum
// ---------------------------------------------------------------------------

fn draw_diff_spectrum(
    painter: &egui::Painter,
    center_freqs: &[f32],
    diff_bins: &[f32],
    rect: egui::Rect,
) {
    let diff_range = 45.0f32;
    let center_y = rect.center().y;
    let half_h = rect.height() / 2.0;

    painter.line_segment(
        [
            egui::pos2(rect.left(), center_y),
            egui::pos2(rect.right(), center_y),
        ],
        egui::Stroke::new(0.5, egui::Color32::from_white_alpha(60)),
    );

    let n = diff_bins.len().min(center_freqs.len());
    let mut stroke_points: Vec<egui::Pos2> = Vec::with_capacity(n);
    let mut last_px = -1i32;

    for i in 0..n {
        let freq = center_freqs[i];
        if freq < FREQ_MIN || freq > FREQ_MAX {
            continue;
        }
        let x = freq_to_x(freq, rect.left(), rect.width());
        let px = x as i32;
        if px == last_px {
            continue;
        }
        last_px = px;

        let clamped = diff_bins[i].clamp(-diff_range, diff_range);
        let y = center_y - (clamped / diff_range) * half_h;
        stroke_points.push(egui::pos2(x, y));
    }

    if stroke_points.len() < 2 {
        return;
    }

    let mut mesh = egui::Mesh::default();
    for (idx, &point) in stroke_points.iter().enumerate() {
        let color = if point.y <= center_y {
            FILL_DIFF_POS
        } else {
            FILL_DIFF_NEG
        };
        mesh.colored_vertex(point, color);
        mesh.colored_vertex(egui::pos2(point.x, center_y), color);
        if idx > 0 {
            let i = (idx * 2) as u32;
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i + 1, i);
        }
    }
    painter.add(egui::Shape::mesh(mesh));

    painter.add(egui::Shape::line(
        stroke_points,
        egui::Stroke::new(1.5, STROKE_GHOST),
    ));
}

// ---------------------------------------------------------------------------
// Legend
// ---------------------------------------------------------------------------

fn draw_legend(painter: &egui::Painter, ui_state: &UiState, spec: egui::Rect) {
    let font = egui::FontId::monospace(10.0);
    let line_h = 14.0;
    let swatch_w = 12.0;
    let pad = 6.0;
    let mut y = spec.top() + pad;
    let x = spec.left() + pad;

    let mut entries: Vec<(&str, egui::Color32)> = Vec::new();

    match ui_state.view_mode {
        ViewMode::Diff => {
            if !ui_state.remotes.is_empty() {
                entries.push(("Boost", STROKE_DIFF_POS));
                entries.push(("Cut", STROKE_DIFF_NEG));
            }
        }
        ViewMode::Normal => {
            if ui_state.spectrum.is_both() {
                entries.push(("L", STROKE_L));
                entries.push(("R", STROKE_R));
            } else {
                entries.push((&ui_state.instance_name, STROKE_L));
            }
        }
        ViewMode::Both => {
            entries.push((&ui_state.instance_name, STROKE_L));
            if !ui_state.remotes.is_empty() {
                entries.push(("Boost", STROKE_DIFF_POS));
                entries.push(("Cut", STROKE_DIFF_NEG));
            }
        }
    }

    for (label, color) in &entries {
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, y + 2.0), egui::vec2(swatch_w, 8.0)),
            0.0,
            *color,
        );
        painter.text(
            egui::pos2(x + swatch_w + 4.0, y),
            egui::Align2::LEFT_TOP,
            *label,
            font.clone(),
            theme::TEXT_DIM,
        );
        y += line_h;
    }

    if matches!(ui_state.view_mode, ViewMode::Normal | ViewMode::Both) {
        for (idx, remote) in ui_state.remotes.iter().enumerate() {
            let (stroke, _) = GHOST_PALETTE[idx % GHOST_PALETTE.len()];
            let name =
                registry::name_of(remote.id).unwrap_or_else(|| format!("#{}", remote.id.0));
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y + 2.0), egui::vec2(swatch_w, 8.0)),
                0.0,
                stroke,
            );
            painter.text(
                egui::pos2(x + swatch_w + 4.0, y),
                egui::Align2::LEFT_TOP,
                &name,
                font.clone(),
                theme::TEXT_DIM,
            );
            y += line_h;
        }
    }
}

// ---------------------------------------------------------------------------
// Axis labels
// ---------------------------------------------------------------------------

fn draw_labels(painter: &egui::Painter, spec: egui::Rect) {
    let font = egui::FontId::monospace(10.0);

    for &db in DB_GRID {
        let y = db_to_y(db, spec.top(), spec.height());
        painter.text(
            egui::pos2(spec.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{}", db as i32),
            font.clone(),
            theme::TEXT_DIM,
        );
    }

    for &freq in FREQ_GRID {
        let x = freq_to_x(freq, spec.left(), spec.width());
        painter.text(
            egui::pos2(x, spec.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            format_freq(freq),
            font.clone(),
            theme::TEXT_DIM,
        );
    }
}

// ---------------------------------------------------------------------------
// Hover crosshair + readout
// ---------------------------------------------------------------------------

fn draw_hover(
    painter: &egui::Painter,
    pos: egui::Pos2,
    ui_state: &UiState,
    rect: egui::Rect,
) {
    let crosshair = egui::Color32::from_white_alpha(40);
    painter.line_segment(
        [
            egui::pos2(pos.x, rect.top()),
            egui::pos2(pos.x, rect.bottom()),
        ],
        egui::Stroke::new(0.5, crosshair),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left(), pos.y),
            egui::pos2(rect.right(), pos.y),
        ],
        egui::Stroke::new(0.5, crosshair),
    );

    let freq = x_to_freq(pos.x, rect.left(), rect.width());
    let bin = ui_state.spectrum.nearest_bin(freq);
    let db_local = ui_state.bins_a[bin];
    let font = egui::FontId::monospace(11.0);

    let freq_str = if freq >= 1000.0 {
        format!("{:.1} kHz", freq / 1000.0)
    } else {
        format!("{:.0} Hz", freq)
    };

    let mut lines: Vec<String> = vec![freq_str];

    if ui_state.spectrum.is_both() && ui_state.view_mode == ViewMode::Normal {
        let db_b = ui_state.bins_b[bin];
        lines.push(format!("L {:.1} dB  R {:.1} dB", db_local, db_b));
    } else {
        lines.push(format!("{}: {:.1} dB", ui_state.instance_name, db_local));
    }

    for remote in &ui_state.remotes {
        let name = registry::name_of(remote.id).unwrap_or_else(|| format!("#{}", remote.id.0));
        let db = remote.bins.get(bin).copied().unwrap_or(DB_FLOOR);
        if matches!(ui_state.view_mode, ViewMode::Diff | ViewMode::Both) {
            let diff = remote.diff_bins.get(bin).copied().unwrap_or(0.0);
            lines.push(format!("{}: {:.1} dB  \u{0394} {:+.1} dB", name, db, diff));
        } else {
            lines.push(format!("{}: {:.1} dB", name, db));
        }
    }

    let text = lines.join("\n");

    let text_width = painter
        .layout_no_wrap(text.clone(), font.clone(), egui::Color32::WHITE)
        .rect
        .width();
    let margin = 10.0;
    let line_h = 14.0;
    let text_height = lines.len() as f32 * line_h;

    let fits_right = pos.x + margin + text_width < rect.right();
    let (anchor_x, align) = if fits_right {
        (pos.x + margin, egui::Align2::LEFT_TOP)
    } else {
        (pos.x - margin, egui::Align2::RIGHT_TOP)
    };

    let anchor_y = if pos.y - text_height - margin < rect.top() {
        pos.y + margin
    } else {
        pos.y - text_height - 4.0
    };

    painter.text(
        egui::pos2(anchor_x, anchor_y),
        align,
        text,
        font,
        egui::Color32::WHITE,
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{cqt_center_frequencies, AnalyzerCore, SpectrumData};
    use std::cell::RefCell;

    #[test]
    fn builds_and_runs() {
        let result = truce_test::render_effect::<Plugin>(512, 44100.0);
        truce_test::assert_no_nans(&result.output);
    }

    // Use low sample rate in tests to keep kernel generation fast in debug builds.
    // At 8kHz: FFT size ~16384 vs ~262144 at 44.1kHz. ~16x faster.
    const TEST_SR: f64 = 22050.0;

    /// Generate pink noise into a spectrum via CQT.
    /// Pink noise = equal energy per octave = flat in CQT.
    /// Uses a Voss-McCartney algorithm (sum of octave-shifted random sources).
    fn generate_pink_noise(spectrum: &Arc<SpectrumData>) {
        let mut core = AnalyzerCore::new(spectrum.clone());
        core.reset(TEST_SR);
        core.wait_for_kernels();

        // Voss-McCartney pink noise: 12 octave rows
        let mut rng = 0x12345678u32;
        let mut rows = [0.0f32; 12];
        let mut running_sum = 0.0f32;

        let xorshift = |state: &mut u32| -> f32 {
            *state ^= *state << 13;
            *state ^= *state >> 17;
            *state ^= *state << 5;
            (*state as f32 / u32::MAX as f32) * 2.0 - 1.0
        };

        // Initialize rows
        for row in &mut rows {
            *row = xorshift(&mut rng);
            running_sum += *row;
        }

        for i in 0..66_000u32 {
            // Update one row per sample based on trailing zeros of counter
            let tz = i.trailing_zeros() as usize;
            if tz < rows.len() {
                running_sum -= rows[tz];
                rows[tz] = xorshift(&mut rng);
                running_sum += rows[tz];
            }
            let white = xorshift(&mut rng);
            let pink = (running_sum + white) / (rows.len() as f32 + 1.0);
            let sample = pink * 0.5;
            core.process_stereo(sample, sample);
        }
    }

    #[test]
    fn gui_screenshot() {
        let freqs = cqt_center_frequencies();
        let spectrum = Arc::new(SpectrumData::new(freqs));
        generate_pink_noise(&spectrum);

        let instance_id = registry::register(Some("Test"), spectrum.clone());
        let ui_cell = RefCell::new(UiState::new(spectrum, instance_id));

        truce_egui::snapshot::assert_snapshot::<TruceAnalyzerParams>(
            "screenshots",
            "analyzer_spectrum",
            800,
            400,
            2.0,
            0,
            Some(truce_gui::font::JETBRAINS_MONO),
            |ctx, state| {
                let mut ui = ui_cell.borrow_mut();
                ui.update_local();
                analyzer_ui(ctx, state, &mut ui);
            },
        );

        registry::deregister(instance_id);
    }


    #[test]
    fn gui_screenshot_diff() {
        let freqs = cqt_center_frequencies();
        let num_bins = freqs.len();

        // "Before EQ" — pink noise (flat in CQT)
        let spec_before = Arc::new(SpectrumData::new(freqs.clone()));
        generate_pink_noise(&spec_before);
        let id_before = registry::register(Some("Before EQ"), spec_before.clone());

        // "After EQ" — simulate a warm EQ: boost lows, gentle mid scoop, cut highs
        let spec_after = Arc::new(SpectrumData::new(freqs));
        for i in 0..num_bins {
            let freq = spec_before.center_freqs_slice()[i];
            let original = spec_before.read_bin(i);
            // Log-linear tilt: +4 dB at 30 Hz, 0 dB at ~500 Hz, -10 dB at 10 kHz
            // Mid scoop centered at 800 Hz + high shelf boost above 4 kHz.
            // Two Gaussian-like bells.
            let scoop = -8.0
                * (-((freq / 800.0).log2()).powi(2) / (2.0 * 0.6_f32.powi(2))).exp();
            let high_boost = 6.0
                * (-((freq / 8000.0).log2()).powi(2) / (2.0 * 0.7_f32.powi(2))).exp();
            let eq_db = scoop + high_boost;
            spec_after.write_bin(i, (original + eq_db).clamp(DB_FLOOR, 0.0));
        }
        spec_after.bump_version();
        let id_after = registry::register(Some("After EQ"), spec_after.clone());

        let ui_cell = RefCell::new({
            let mut ui = UiState::new(spec_after, id_after);
            ui.instance_name = "After EQ".to_string();
            ui.selected_ids.push(id_before);
            ui.view_mode = ViewMode::Both;
            ui.spectrum.set_has_remotes(true);
            ui
        });

        truce_egui::snapshot::assert_snapshot::<TruceAnalyzerParams>(
            "screenshots",
            "analyzer_diff",
            800,
            400,
            2.0,
            0,
            Some(truce_gui::font::JETBRAINS_MONO),
            |ctx, state| {
                let mut ui = ui_cell.borrow_mut();
                ui.update_local();
                ui.update_remotes();
                ui.update_diff();
                analyzer_ui(ctx, state, &mut ui);
            },
        );

        registry::deregister(id_before);
        registry::deregister(id_after);
    }

    #[test]
    fn registry_round_trip() {
        let freqs = cqt_center_frequencies();
        let spectrum = Arc::new(SpectrumData::new(freqs));
        let id = registry::register(Some("Test Instance"), spectrum.clone());

        let list = registry::list();
        assert!(list.iter().any(|(i, n)| *i == id && n == "Test Instance"));
        assert!(registry::get(id).is_some());

        registry::rename(id, "Renamed");
        assert_eq!(registry::name_of(id), Some("Renamed".to_string()));
        assert_eq!(registry::find_by_name("Renamed"), Some(id));

        registry::deregister(id);
        assert!(registry::get(id).is_none());
    }
}
