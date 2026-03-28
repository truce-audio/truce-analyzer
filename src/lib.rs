mod core;

use std::sync::Arc;

use truce::prelude::*;
use truce_egui::{EguiEditor, ParamState};

use crate::core::{
    cqt_center_frequencies, db_to_y, format_freq, freq_to_x, x_to_freq, AnalyzerCore,
    SpectrumData, DB_FLOOR, DB_GRID, FREQ_GRID, FREQ_MAX, FREQ_MIN, MODE_BOTH, MODE_DIFF,
    MODE_LEFT, MODE_RIGHT, MODE_SUM,
};

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(ParamEnum)]
pub enum ChannelMode {
    Both,
    Left,
    Right,
    Sum,
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
}

impl TruceAnalyzer {
    pub fn new(params: Arc<TruceAnalyzerParams>) -> Self {
        let freqs = cqt_center_frequencies();
        let spectrum = Arc::new(SpectrumData::new(freqs));
        Self {
            params,
            core: AnalyzerCore::new(spectrum),
        }
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
        let mode = self.params.channel.value().as_mode_u8();
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

    fn custom_editor(&self) -> Option<Box<dyn truce_core::editor::Editor>> {
        let spectrum = self.core.spectrum().clone();
        let num_bins = spectrum.num_bins();
        let mut bins_a = vec![DB_FLOOR; num_bins];
        let mut bins_b = vec![DB_FLOOR; num_bins];

        Some(Box::new(
            EguiEditor::new(
                (800, 400),
                move |ctx: &egui::Context, state: &ParamState| {
                    spectrum.read_all(&mut bins_a);
                    if spectrum.is_both() {
                        spectrum.read_all_b(&mut bins_b);
                    }
                    analyzer_ui(ctx, state, &spectrum, &bins_a, &bins_b);
                },
            )
            .with_visuals(truce_egui::theme::dark())
            .with_font(truce_gui::font::JETBRAINS_MONO),
        ))
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
const BOTTOM_MARGIN: f32 = 20.0;

const BG_COLOR: egui::Color32 = egui::Color32::from_rgb(26, 26, 46);
const HEADER_BG: egui::Color32 = egui::Color32::from_rgb(18, 18, 42);
const HEADER_TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(106, 176, 255);
const GRID_COLOR: egui::Color32 = egui::Color32::from_rgb(42, 42, 74);
const LABEL_COLOR: egui::Color32 = egui::Color32::from_rgb(136, 136, 153);

const STROKE_L: egui::Color32 = egui::Color32::from_rgb(106, 176, 255);
const FILL_L: egui::Color32 = egui::Color32::from_rgba_premultiplied(30, 58, 87, 50);
const STROKE_R: egui::Color32 = egui::Color32::from_rgb(255, 140, 90);
const FILL_R: egui::Color32 = egui::Color32::from_rgba_premultiplied(87, 45, 25, 50);

fn analyzer_ui(
    ctx: &egui::Context,
    state: &ParamState,
    spectrum: &SpectrumData,
    bins_a: &[f32],
    bins_b: &[f32],
) {
    egui::TopBottomPanel::top("header")
        .exact_height(30.0)
        .frame(egui::Frame::NONE.fill(HEADER_BG))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("TRUCE ANALYZER")
                        .size(14.0)
                        .color(HEADER_TEXT_COLOR)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    let channel_id: u32 = P::Channel.into();
                    let current = state.format(channel_id);
                    let options = ["Both", "Left", "Right", "Sum", "Diff"];
                    egui::ComboBox::from_id_salt("channel_mode")
                        .selected_text(current)
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            for (i, &label) in options.iter().enumerate() {
                                let norm = i as f64 / (options.len() - 1) as f64;
                                let selected = (state.get(channel_id) - norm).abs() < 0.01;
                                if ui.selectable_label(selected, label).clicked() {
                                    state.set_immediate(channel_id, norm);
                                }
                            }
                        });
                });
            });
        });

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(BG_COLOR))
        .show(ctx, |ui| {
            let (response, painter) =
                ui.allocate_painter(ui.available_size(), egui::Sense::hover());
            let rect = response.rect;

            let spec = egui::Rect::from_min_max(
                egui::pos2(rect.left() + LEFT_MARGIN, rect.top()),
                egui::pos2(rect.right(), rect.bottom() - BOTTOM_MARGIN),
            );

            draw_grid(&painter, spec);

            if spectrum.is_both() {
                draw_spectrum(&painter, spectrum, bins_b, spec, STROKE_R, FILL_R);
            }
            draw_spectrum(&painter, spectrum, bins_a, spec, STROKE_L, FILL_L);

            draw_labels(&painter, spec);

            if let Some(pos) = response.hover_pos() {
                if spec.contains(pos) {
                    draw_hover(&painter, pos, spectrum, bins_a, bins_b, spec);
                }
            }
        });

    ctx.request_repaint();
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
    spectrum: &SpectrumData,
    bins: &[f32],
    rect: egui::Rect,
    stroke_color: egui::Color32,
    fill_color: egui::Color32,
) {
    let mut curve_points: Vec<egui::Pos2> = Vec::with_capacity(bins.len());

    for (i, &db) in bins.iter().enumerate() {
        let freq = spectrum.center_freq(i);
        if freq < FREQ_MIN || freq > FREQ_MAX {
            continue;
        }
        let x = freq_to_x(freq, rect.left(), rect.width());
        let y = db_to_y(db, rect.top(), rect.height());
        curve_points.push(egui::pos2(x, y));
    }

    if curve_points.len() < 2 {
        return;
    }

    // Filled area as a triangle-strip mesh
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

    // Stroke on top
    painter.add(egui::Shape::line(
        curve_points,
        egui::Stroke::new(1.5, stroke_color),
    ));
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
            LABEL_COLOR,
        );
    }

    for &freq in FREQ_GRID {
        let x = freq_to_x(freq, spec.left(), spec.width());
        painter.text(
            egui::pos2(x, spec.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            format_freq(freq),
            font.clone(),
            LABEL_COLOR,
        );
    }
}

// ---------------------------------------------------------------------------
// Hover crosshair + readout
// ---------------------------------------------------------------------------

fn draw_hover(
    painter: &egui::Painter,
    pos: egui::Pos2,
    spectrum: &SpectrumData,
    bins_a: &[f32],
    bins_b: &[f32],
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
    let bin = spectrum.nearest_bin(freq);
    let db_a = bins_a[bin];

    let freq_str = if freq >= 1000.0 {
        format!("{:.1} kHz", freq / 1000.0)
    } else {
        format!("{:.0} Hz", freq)
    };

    let label = if spectrum.is_both() {
        let db_b = bins_b[bin];
        format!("{}  L {:.1} dB  R {:.1} dB", freq_str, db_a, db_b)
    } else {
        format!("{}  {:.1} dB", freq_str, db_a)
    };

    painter.text(
        egui::pos2(pos.x + 10.0, pos.y - 4.0),
        egui::Align2::LEFT_BOTTOM,
        label,
        egui::FontId::monospace(11.0),
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

    fn generate_test_signal(spectrum: &Arc<SpectrumData>) {
        let mut core = AnalyzerCore::new(spectrum.clone());
        core.reset(44100.0);
        let sr = 44100.0f32;
        let pi2 = 2.0 * std::f32::consts::PI;
        for i in 0..135_000 {
            let t = i as f32 / sr;
            let signal = 0.30 * (pi2 * 100.0 * t).sin()
                + 0.30 * (pi2 * 440.0 * t).sin()
                + 0.20 * (pi2 * 1000.0 * t).sin()
                + 0.10 * (pi2 * 5000.0 * t).sin()
                + 0.10 * (pi2 * 10000.0 * t).sin();
            core.process_stereo(signal, signal);
        }
    }

    #[test]
    fn gui_screenshot() {
        let freqs = cqt_center_frequencies();
        let spectrum = Arc::new(SpectrumData::new(freqs));
        generate_test_signal(&spectrum);

        let num_bins = spectrum.num_bins();
        let bins_a = RefCell::new(vec![DB_FLOOR; num_bins]);
        let bins_b = RefCell::new(vec![DB_FLOOR; num_bins]);

        truce_egui::snapshot::assert_snapshot(
            "screenshots",
            "analyzer_spectrum",
            800,
            400,
            2.0,
            0,
            Some(truce_gui::font::JETBRAINS_MONO),
            |ctx, state| {
                let mut a = bins_a.borrow_mut();
                let mut b = bins_b.borrow_mut();
                spectrum.read_all(&mut a);
                if spectrum.is_both() {
                    spectrum.read_all_b(&mut b);
                }
                analyzer_ui(ctx, state, &spectrum, &a, &b);
            },
        );
    }

}
