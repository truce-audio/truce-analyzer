# Slint GUI: Spectrum Analyzer Design

## Overview

The slint variant uses `truce-slint` with Slint's declarative markup language. Slint's strength is in declarative UI definitions with property bindings, but custom real-time rendering (like a spectrum curve) requires a different strategy than the widget-oriented approach Slint is designed for.

## Approach

The plugin returns a `SlintEditor` from `custom_editor()`:

```rust
impl PluginLogic for TruceAnalyzerSlint {
    fn custom_editor(&self) -> Option<Box<dyn Editor>> {
        let spectrum = self.spectrum_data.clone();

        Some(Box::new(
            SlintEditor::new((800, 400), move |state: ParamState| {
                let ui = AnalyzerUi::new().unwrap();
                setup_bindings(&ui, &state, &spectrum);

                let spectrum = spectrum.clone();
                Box::new(move |state: &ParamState| {
                    update_spectrum(&ui, &spectrum);
                })
            })
        ))
    }
}
```

## Window Size

```
800 x 400 logical pixels
```

## Slint Markup

Slint UIs are defined in `.slint` files compiled at build time. The challenge is that Slint's built-in elements (Rectangle, Text, Path) are declarative and property-bound — not designed for data-driven polylines with hundreds of points.

### Strategy: Image-Based Rendering

The most practical approach is to **render the spectrum to a pixel buffer in Rust** and pass it to Slint as an `Image`:

```slint
// ui/analyzer.slint

export component AnalyzerUi inherits Window {
    width: 800px;
    height: 400px;
    background: #1a1a2e;

    // Header
    Rectangle {
        height: 30px;
        background: #12122a;

        Text {
            text: "TRUCE ANALYZER";
            color: #6ab0ff;
            font-size: 14px;
            x: 10px;
            vertical-alignment: center;
        }
    }

    // Spectrum display (rendered as image from Rust)
    spectrum-image := Image {
        y: 30px;
        width: parent.width;
        height: parent.height - 30px;
        source: @image-url("");  // placeholder, set from Rust
    }

    // Exported properties
    in property <image> spectrum-source;

    // Bind image
    spectrum-image.source: spectrum-source;
}
```

### Rust-Side Rendering

```rust
fn update_spectrum(ui: &AnalyzerUi, spectrum: &SpectrumData) {
    let width = 800u32;
    let height = 370u32;  // minus header
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    // Render spectrum into RGBA pixel buffer
    render_spectrum_to_buffer(&mut pixels, width, height, spectrum);

    // Create Slint image from pixel buffer
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &pixels, width, height,
    );
    let image = slint::Image::from_rgba8(buffer);
    ui.set_spectrum_source(image);
}
```

The `render_spectrum_to_buffer` function draws into a raw RGBA buffer. This can use:
- **tiny-skia** directly (the same library truce's CpuBackend uses)
- A minimal software rasterizer (line drawing + rect fills)
- The truce `CpuBackend` itself if it exposes pixel buffer access

### Alternative: Slint Path Element

Slint has a `Path` element that can draw SVG-like paths. This could theoretically render the spectrum curve:

```slint
Path {
    width: parent.width;
    height: parent.height;
    stroke: #6ab0ff;
    stroke-width: 2px;
    commands: root.spectrum-path-data;  // SVG path string
}
```

Where `spectrum-path-data` is an SVG path string like `"M 0 370 L 5 350 L 10 320 ..."` updated from Rust every frame.

**Tradeoffs:**
- Pro: Pure Slint rendering, no pixel buffer management
- Con: Generating and parsing a 2000+ point SVG path string every frame may have performance overhead
- Con: No fill support for the area under the curve (Path only supports stroke)
- Con: Grid lines and labels would need separate Path/Text elements

**Recommendation: Image-based rendering** is more practical for this use case. It gives full control and avoids string-serialization overhead.

## Build Configuration

Slint files are compiled at build time. Add to `build.rs`:

```rust
fn main() {
    slint_build::compile("ui/analyzer.slint").unwrap();
}
```

And in `Cargo.toml`:

```toml
[build-dependencies]
slint-build = "1"

[dependencies]
slint = "1"
```

## Data Flow

```rust
pub struct TruceAnalyzerSlint {
    params: Arc<AnalyzerParams>,
    core: AnalyzerCore,
    spectrum_data: Arc<SpectrumData>,
}
```

The `SlintEditor` closure captures `Arc<SpectrumData>`. The sync closure (returned from the setup function) runs every frame (~60fps via truce's `idle()` loop), reads the latest spectrum, renders to a pixel buffer, and pushes it to the Slint UI via `set_spectrum_source()`.

## Grid and Labels

Since we're rendering to a pixel buffer, grid lines and axis labels are drawn in Rust code, not in Slint markup. This keeps all visualization logic in one place.

Alternatively, overlay Slint `Text` elements for labels:

```slint
// dB labels (positioned absolutely)
Text { x: 4px; y: 30px; text: "0dB"; color: #888899; font-size: 10px; }
Text { x: 4px; y: 73px; text: "-12dB"; color: #888899; font-size: 10px; }
// ... etc

// Hz labels (positioned absolutely)
Text { x: 45px; y: 380px; text: "100"; color: #888899; font-size: 10px; }
// ... etc
```

This gives crisper text than pixel-buffer rendering but requires manual positioning that must stay in sync with the Rust rendering coordinates.

## Hover Interaction

Slint supports pointer events on elements:

```slint
touch := TouchArea {
    x: 0;
    y: 30px;
    width: parent.width;
    height: parent.height - 30px;

    moved => {
        // Expose cursor position to Rust via callback or property
        root.cursor-x = self.mouse-x;
        root.cursor-y = self.mouse-y;
    }
}

out property <float> cursor-x;
out property <float> cursor-y;
```

Rust reads cursor position and renders crosshair/tooltip into the pixel buffer on the next frame.

## Pros and Cons

**Pros:**
- Declarative header/chrome defined in `.slint` markup — clean separation
- IDE preview for static UI elements
- Build-time compilation catches layout errors early
- Software rendering (no GPU required) — same as builtin
- Slint's property binding works well for parameter controls (gain knob, etc.)

**Cons:**
- Custom real-time visualization doesn't play to Slint's strengths
- Image-based rendering is essentially bypassing Slint's rendering for the main content
- Extra pixel buffer allocation + copy every frame
- Two rendering systems in play (Slint for chrome, pixel buffer for spectrum)
- SVG Path alternative has performance and feature limitations
- Most ceremony for least benefit compared to other frameworks for this use case

## Estimated Complexity

Medium-high. The Slint markup itself is simple, but the image-based rendering bridge adds a layer of indirection. The pixel buffer rendering is essentially writing a mini software renderer (or pulling in tiny-skia), which duplicates work that egui/iced/builtin handle natively.

## When Slint Shines

Slint would be the better choice if the analyzer had a complex control panel (knobs, dropdowns, tabs, text fields) alongside a small visualization area. For a plugin that is primarily a full-window visualization, the other frameworks are a more natural fit.
