# egui GUI: Spectrum Analyzer Design

## Overview

The egui variant uses `truce-egui` with egui's immediate-mode API. egui excels at custom painting via its `Painter` API, which provides arbitrary shape rendering (lines, polygons, Bezier curves, meshes). This makes it the most flexible option for rich visualizations.

## Approach

The plugin returns an `EguiEditor` from `custom_editor()`:

```rust
impl PluginLogic for TruceAnalyzerEgui {
    fn custom_editor(&self) -> Option<Box<dyn Editor>> {
        let spectrum = self.spectrum_data.clone();  // Arc<SpectrumData>

        Some(Box::new(
            EguiEditor::new((800, 400), move |ctx: &egui::Context, state: &ParamState| {
                analyzer_ui(ctx, state, &spectrum);
            })
            .with_visuals(truce_egui::theme::dark())
            .with_font(truce_gui::font::JETBRAINS_MONO)
        ))
    }
}
```

## Window Size

```
800 x 400 logical pixels
```

egui handles DPI scaling automatically via its `Context`.

## UI Structure

```rust
fn analyzer_ui(ctx: &egui::Context, state: &ParamState, spectrum: &SpectrumData) {
    egui::TopBottomPanel::top("header").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading("TRUCE ANALYZER");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Future: controls (FFT size, channel mode)
            });
        });
    });

    egui::CentralPanel::default().show(ctx, |ui| {
        draw_spectrum(ui, spectrum);
    });

    // Request continuous repaint for animation
    ctx.request_repaint();
}
```

## Spectrum Rendering with egui::Painter

egui's `Painter` is the key advantage. It supports filled polygons, polylines, and meshes natively.

### Filled Spectrum Curve

```rust
fn draw_spectrum(ui: &mut egui::Ui, spectrum: &SpectrumData) {
    let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::hover());
    let rect = response.rect;

    // 1. Background
    painter.rect_filled(rect, 0.0, Color32::from_rgb(26, 26, 46));

    // 2. Grid
    draw_grid(&painter, rect);

    // 3. Build spectrum polygon
    let bins = spectrum.read();
    let mut points: Vec<Pos2> = Vec::with_capacity(bins.len() + 2);

    // Start at bottom-left
    points.push(pos2(rect.left(), rect.bottom()));

    for i in 0..bins.len() {
        let freq = spectrum.bin_frequency(i);
        if freq < 20.0 || freq > 20_000.0 { continue; }
        let x = freq_to_x(freq, rect);
        let y = db_to_y(bins[i], rect);
        points.push(pos2(x, y));
    }

    // Close at bottom-right
    points.push(pos2(rect.right(), rect.bottom()));

    // 4. Draw filled polygon
    let fill_color = Color32::from_rgba_unmultiplied(74, 144, 217, 80);
    painter.add(Shape::convex_polygon(points.clone(), fill_color, Stroke::NONE));

    // 5. Draw stroke on top (just the curve, not the closing edges)
    let curve_points: Vec<Pos2> = points[1..points.len()-1].to_vec();
    let stroke = Stroke::new(2.0, Color32::from_rgb(106, 176, 255));
    painter.add(Shape::line(curve_points, stroke));
}
```

### Grid Lines and Labels

```rust
fn draw_grid(painter: &egui::Painter, rect: Rect) {
    let grid_color = Color32::from_rgb(42, 42, 74);
    let label_color = Color32::from_rgb(136, 136, 153);

    // Horizontal: dB lines
    for db in (-84..=0).step_by(12) {
        let y = db_to_y(db as f32, rect);
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)],
                             Stroke::new(0.5, grid_color));
        painter.text(pos2(rect.left() + 4.0, y - 7.0), Align2::LEFT_TOP,
                     format!("{}dB", db), FontId::monospace(10.0), label_color);
    }

    // Vertical: frequency lines
    for &freq in &[50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
        let x = freq_to_x(freq, rect);
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())],
                             Stroke::new(0.5, grid_color));
        let label = if freq >= 1000.0 { format!("{}k", freq / 1000.0) }
                    else { format!("{}", freq) };
        painter.text(pos2(x, rect.bottom() - 14.0), Align2::CENTER_TOP,
                     label, FontId::monospace(10.0), label_color);
    }
}
```

## Hover Interaction

egui makes hover feedback trivial:

```rust
if let Some(pos) = response.hover_pos() {
    let freq = x_to_freq(pos.x, rect);
    let bin = (freq / bin_width).round() as usize;
    let db = bins.get(bin).copied().unwrap_or(DB_FLOOR);

    // Crosshair
    painter.line_segment([pos2(pos.x, rect.top()), pos2(pos.x, rect.bottom())],
                         Stroke::new(0.5, Color32::from_white_alpha(60)));

    // Tooltip
    let text = format!("{:.0} Hz  {:.1} dB", freq, db);
    painter.text(pos2(pos.x + 8.0, pos.y - 16.0), Align2::LEFT_BOTTOM,
                 text, FontId::monospace(11.0), Color32::WHITE);
}
```

## Data Flow

```rust
pub struct TruceAnalyzerEgui {
    params: Arc<AnalyzerParams>,
    core: AnalyzerCore,
    spectrum_data: Arc<SpectrumData>,  // cloned into EguiEditor closure
}
```

The `EguiEditor` closure captures an `Arc<SpectrumData>`. On each frame, it calls `spectrum.read()` to get the latest dB values. The `Arc` is shared between the plugin struct (audio thread writes) and the editor closure (GUI thread reads), connected by the internal triple-buffer.

## Continuous Repaint

egui only repaints on events by default. Since the spectrum is animated, call `ctx.request_repaint()` every frame to ensure continuous updates. This is standard practice for real-time visualizations in egui.

## Pros and Cons

**Pros:**
- Full `Painter` API: filled polygons, polylines, bezier curves, meshes
- Hover/tooltip interaction is trivial (immediate-mode)
- Mature ecosystem (egui is widely used)
- GPU-accelerated via egui-wgpu
- Easiest path to rich interactivity (markers, zoom, click-to-freeze)

**Cons:**
- Largest binary size (egui + wgpu)
- GPU required (wgpu backend)
- Immediate-mode redraws entire UI every frame (fine for an analyzer that needs continuous repaint anyway)
- Learning curve if unfamiliar with egui

## Estimated Complexity

Low. egui's `Painter` API maps directly to the visualization needs. The filled polygon + stroke line approach is clean and egui handles DPI, event routing, and frame pacing.
