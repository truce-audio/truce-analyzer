# Builtin GUI: Spectrum Analyzer Design

## Overview

The builtin variant uses truce's built-in `RenderBackend` trait with custom rendering. This is the lightest-weight option — no external GUI framework, no GPU requirement. The plugin implements `uses_custom_render() -> true` and draws the spectrum directly via `render()` using the backend's primitive drawing operations.

## Approach

The builtin GUI does not use `GridLayout` for the spectrum. Instead, it opts into **custom rendering** via the `PluginLogic` trait:

```rust
impl PluginLogic for TruceAnalyzerBuiltin {
    fn uses_custom_render(&self) -> bool {
        true
    }

    fn render(&self, backend: &mut dyn RenderBackend) {
        draw_spectrum(backend, &self.spectrum_data, self.width, self.height);
    }
}
```

This gives full control over the pixel output while remaining backend-agnostic — the same code renders on `CpuBackend` (tiny-skia) or `WgpuBackend` (Metal/Vulkan/DX12).

## Window Size

```rust
const WIDTH: u32 = 800;
const HEIGHT: u32 = 400;
```

Layout:
- Header bar: 30px top (plugin name, version)
- Left margin: 45px (dB labels)
- Bottom margin: 25px (Hz labels)
- Spectrum area: remaining space (~710 x 345 px)

## Rendering Implementation

### Available Primitives

The `RenderBackend` trait provides:
- `clear(color)` — background fill
- `fill_rect(x, y, w, h, color)` — rectangles (grid lines, header)
- `draw_line(x1, y1, x2, y2, color, width)` — lines (spectrum curve segments, grid)
- `draw_text(text, x, y, size, color)` — labels (axis text)
- `text_width(text, size) -> f32` — text measurement
- `fill_circle`, `stroke_circle`, `stroke_arc` — available but unlikely needed

### Drawing Steps

```rust
fn draw_spectrum(
    backend: &mut dyn RenderBackend,
    spectrum: &SpectrumData,
    width: f32,
    height: f32,
) {
    // 1. Background
    backend.clear(Color::from_hex(0x1a1a2e));

    // 2. Header bar
    backend.fill_rect(0.0, 0.0, width, 30.0, Color::from_hex(0x12122a));
    backend.draw_text("TRUCE ANALYZER", 10.0, 8.0, 14.0, Color::from_hex(0x6ab0ff));

    // 3. Grid lines
    let area = SpectrumArea { x: 45.0, y: 30.0, w: width - 55.0, h: height - 55.0 };
    draw_grid(backend, &area);

    // 4. Spectrum curve (line segments between adjacent bins)
    let bins = spectrum.read();
    let mut prev_x = area.x;
    let mut prev_y = db_to_y(bins[0], &area);

    for i in 1..bins.len() {
        let freq = spectrum.bin_frequency(i);
        if freq < 20.0 || freq > 20_000.0 { continue; }

        let x = freq_to_x(freq, &area);
        let y = db_to_y(bins[i], &area);
        backend.draw_line(prev_x, prev_y, x, y, Color::from_rgba(0.42, 0.69, 1.0, 0.9), 1.5);
        prev_x = x;
        prev_y = y;
    }

    // 5. Axis labels
    draw_axis_labels(backend, &area);
}
```

### Coordinate Helpers

```rust
fn freq_to_x(freq: f32, area: &SpectrumArea) -> f32 {
    let log_min = 20_f32.ln();
    let log_max = 20_000_f32.ln();
    let t = (freq.ln() - log_min) / (log_max - log_min);
    area.x + t * area.w
}

fn db_to_y(db: f32, area: &SpectrumArea) -> f32 {
    let t = (db - DB_FLOOR) / (DB_CEIL - DB_FLOOR);  // 0..1 from floor to ceil
    area.y + area.h * (1.0 - t)                       // flip: 0dB at top
}
```

## Filled Spectrum

The `RenderBackend` does not have a `fill_polygon` primitive. To create a filled spectrum effect:

**Approach: Vertical line strips**
For each x-pixel column, draw a thin vertical `fill_rect` from the spectrum curve's y-value down to the bottom of the area. This creates a filled appearance with minimal per-frame cost.

```rust
for px in 0..area.w as usize {
    let freq = x_to_freq(area.x + px as f32, &area);
    let bin = (freq / bin_width).round() as usize;
    let db = bins.get(bin).copied().unwrap_or(DB_FLOOR);
    let y = db_to_y(db, &area);
    let bar_h = (area.y + area.h) - y;
    backend.fill_rect(
        area.x + px as f32, y, 1.0, bar_h,
        Color::from_rgba(0.29, 0.56, 0.85, 0.3),
    );
}
```

## Data Flow

```rust
pub struct TruceAnalyzerBuiltin {
    params: Arc<AnalyzerParams>,
    core: AnalyzerCore,                          // DSP: ring buffer + FFT
    spectrum_data: Arc<SpectrumData>,             // shared with render()
}
```

Since `render()` takes `&self`, the `SpectrumData` must be readable via shared reference. The triple-buffer receiver can be wrapped in an `Arc<Mutex<_>>` (the GUI thread lock is uncontested — audio never touches it) or use the atomic array approach from the main design doc.

## Interaction

The builtin editor supports mouse interaction via `hit_test()` and `on_mouse_*` callbacks on `BuiltinEditor`. For v0.1, the analyzer is display-only — no mouse interaction needed. Future versions could add:
- Hover to show frequency/amplitude at cursor
- Click to place a marker

## Pros and Cons

**Pros:**
- Zero external GUI dependencies
- Works on CPU-only (tiny-skia) and GPU (wgpu) identically
- Smallest binary size
- Hot-reload friendly — render() is part of PluginLogic

**Cons:**
- No high-level layout system — all coordinates are manual
- No fill_polygon — filled spectrum requires vertical-strip workaround
- No built-in text input or scrolling (not needed for analyzer)
- Anti-aliasing quality depends on backend (tiny-skia is decent, wgpu depends on tessellation)

## Estimated Complexity

Low-medium. The `RenderBackend` API is simple and the drawing is straightforward line/rect work. The main effort is the coordinate mapping math and making the grid look clean.
