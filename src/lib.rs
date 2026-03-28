mod core;

use std::sync::Arc;

use truce::prelude::*;
use truce_egui::{EguiEditor, ParamState};

use crate::core::{
    cqt_center_frequencies, db_to_y, format_freq, freq_to_x, x_to_freq, AnalyzerCore,
    SpectrumData, DB_FLOOR, DB_GRID, FREQ_GRID, FREQ_MAX, FREQ_MIN,
};

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(Params)]
pub struct TruceAnalyzerParams {
    #[param(name = "Gain", range = "linear(-60, 6)", unit = "dB", smooth = "exp(5)")]
    pub gain: FloatParam,
}

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
        let channels = buffer.channels().max(1);
        for i in 0..buffer.num_samples() {
            let gain = db_to_linear(self.params.gain.smoothed_next() as f64) as f32;
            let mut mono_sum = 0.0f32;
            for ch in 0..channels {
                let (inp, out) = buffer.io(ch);
                out[i] = inp[i] * gain;
                mono_sum += out[i];
            }
            self.core.process_sample(mono_sum / channels as f32);
        }
        ProcessStatus::Normal
    }

    fn custom_editor(&self) -> Option<Box<dyn truce_core::editor::Editor>> {
        let spectrum = self.core.spectrum().clone();
        let num_bins = spectrum.num_bins();
        let mut bins = vec![DB_FLOOR; num_bins];

        Some(Box::new(
            EguiEditor::new(
                (800, 400),
                move |ctx: &egui::Context, _state: &ParamState| {
                    spectrum.read_all(&mut bins);
                    analyzer_ui(ctx, &spectrum, &bins);
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
const STROKE_COLOR: egui::Color32 = egui::Color32::from_rgb(106, 176, 255);
const FILL_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(30, 58, 87, 50);

fn analyzer_ui(ctx: &egui::Context, spectrum: &SpectrumData, bins: &[f32]) {
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
            draw_spectrum(&painter, spectrum, bins, spec);
            draw_labels(&painter, spec);

            if let Some(pos) = response.hover_pos() {
                if spec.contains(pos) {
                    draw_hover(&painter, pos, spectrum, bins, spec);
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
        mesh.colored_vertex(point, FILL_COLOR);
        mesh.colored_vertex(egui::pos2(point.x, rect.bottom()), FILL_COLOR);
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
        egui::Stroke::new(1.5, STROKE_COLOR),
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
    bins: &[f32],
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
    let db = bins[bin];

    let freq_str = if freq >= 1000.0 {
        format!("{:.1} kHz", freq / 1000.0)
    } else {
        format!("{:.0} Hz", freq)
    };
    let label = format!("{}  {:.1} dB", freq_str, db);

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

    #[test]
    fn builds_and_runs() {
        let result = truce_test::render_effect::<Plugin>(512, 44100.0);
        truce_test::assert_no_nans(&result.output);
    }
}
