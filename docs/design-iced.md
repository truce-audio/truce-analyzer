# Iced GUI: Spectrum Analyzer Design

## Overview

The iced variant uses `truce-iced` with iced's retained-mode Elm architecture. Iced provides a `Canvas` widget for custom 2D rendering, which is the primary tool for drawing the spectrum. The architecture uses message-passing for state updates and a declarative `view()` function for layout.

## Approach

The plugin uses iced's **custom mode** via the `IcedPlugin` trait, returning an `IcedEditor` from `custom_editor()`:

```rust
impl PluginLogic for TruceAnalyzerIced {
    fn custom_editor(&self) -> Option<Box<dyn Editor>> {
        Some(Box::new(
            IcedEditor::<AnalyzerParams, AnalyzerUi>::new(
                Arc::new(AnalyzerParams::default_for_gui()),
                (800, 400),
            )
            .with_font("JetBrains Mono", truce_gui::font::JETBRAINS_MONO)
        ))
    }
}
```

## Window Size

```
800 x 400 logical pixels
```

Iced handles DPI scaling via its renderer.

## Elm Architecture

Iced uses the Elm pattern: **Model -> Message -> Update -> View**.

### Messages

```rust
#[derive(Debug, Clone)]
pub enum AnalyzerMsg {
    Tick,  // periodic repaint trigger
}
```

### Plugin Implementation

```rust
pub struct AnalyzerUi {
    spectrum: Arc<SpectrumData>,
    cache: canvas::Cache,  // iced canvas cache for redraw optimization
}

impl IcedPlugin<AnalyzerParams> for AnalyzerUi {
    type Message = AnalyzerMsg;

    fn new(params: Arc<AnalyzerParams>) -> Self {
        // spectrum_data would need to be passed via shared state
        Self {
            spectrum: /* shared SpectrumData */,
            cache: canvas::Cache::new(),
        }
    }

    fn update(
        &mut self,
        message: AnalyzerMsg,
        _params: &ParamState,
        _ctx: &EditorHandle,
    ) -> Task<Message<AnalyzerMsg>> {
        match message {
            AnalyzerMsg::Tick => {
                self.cache.clear();  // invalidate canvas to trigger redraw
                Task::none()
            }
        }
    }

    fn view<'a>(
        &'a self,
        params: &'a ParamState,
    ) -> Element<'a, Message<AnalyzerMsg>> {
        let header = text("TRUCE ANALYZER").size(16);

        let spectrum_canvas = Canvas::new(&self.spectrum_view)
            .width(Length::Fill)
            .height(Length::Fill);

        column![header, spectrum_canvas].into()
    }
}
```

## Canvas Rendering

Iced's `Canvas` widget is the core rendering mechanism. It implements the `Program` trait:

```rust
struct SpectrumView {
    spectrum: Arc<SpectrumData>,
}

impl canvas::Program<Message<AnalyzerMsg>> for SpectrumView {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let frame = canvas::Frame::new(renderer, bounds.size());

        // 1. Background
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgb8(26, 26, 46),
        );

        // 2. Grid lines
        draw_grid(&frame, bounds);

        // 3. Spectrum path
        let bins = self.spectrum.read();
        let mut builder = canvas::path::Builder::new();

        // Start at bottom-left
        builder.move_to(Point::new(0.0, bounds.height));

        for i in 0..bins.len() {
            let freq = self.spectrum.bin_frequency(i);
            if freq < 20.0 || freq > 20_000.0 { continue; }
            let x = freq_to_x(freq, bounds.width);
            let y = db_to_y(bins[i], bounds.height);
            builder.line_to(Point::new(x, y));
        }

        // Close at bottom-right
        builder.line_to(Point::new(bounds.width, bounds.height));
        builder.close();

        let path = builder.build();

        // 4. Fill
        frame.fill(&path, Color::from_rgba8(74, 144, 217, 0.3));

        // 5. Stroke (curve only, rebuild without closing edges)
        let mut curve_builder = canvas::path::Builder::new();
        let mut first = true;
        for i in 0..bins.len() {
            let freq = self.spectrum.bin_frequency(i);
            if freq < 20.0 || freq > 20_000.0 { continue; }
            let x = freq_to_x(freq, bounds.width);
            let y = db_to_y(bins[i], bounds.height);
            if first { curve_builder.move_to(Point::new(x, y)); first = false; }
            else { curve_builder.line_to(Point::new(x, y)); }
        }

        frame.stroke(
            &curve_builder.build(),
            canvas::Stroke::default()
                .with_color(Color::from_rgb8(106, 176, 255))
                .with_width(2.0),
        );

        vec![frame.into_geometry()]
    }
}
```

## Continuous Animation

Iced is event-driven — the canvas only redraws when its cache is cleared. For continuous animation, the plugin needs a periodic tick. Options:

**Option A — Subscription (preferred)**
Iced supports `Subscription` for async event streams. A time subscription can fire at ~60fps:

```rust
fn subscription(&self) -> Subscription<Message<AnalyzerMsg>> {
    iced::time::every(Duration::from_millis(16))
        .map(|_| Message::Custom(AnalyzerMsg::Tick))
}
```

**Option B — Cache invalidation in idle**
If truce-iced calls `idle()` at ~60fps, the canvas cache can be cleared there, forcing a redraw each frame.

## Data Flow Challenge

The main architectural consideration with iced is getting the `SpectrumData` into the `IcedPlugin` impl. The `new()` constructor receives `Arc<AnalyzerParams>` but not arbitrary shared state.

**Solution: Embed SpectrumData in params**

Add an `Arc<SpectrumData>` field to the params struct (not a `#[param]` — just a plain field). The `#[derive(Params)]` macro ignores non-annotated fields:

```rust
#[derive(Params)]
pub struct AnalyzerParams {
    #[param(name = "Gain", range = "linear(-60, 6)", unit = "dB", smooth = "exp(5)")]
    pub gain: FloatParam,

    // Not a parameter — shared spectrum data for GUI
    #[param(skip)]
    pub spectrum: Arc<SpectrumData>,
}
```

If `#[param(skip)]` is not supported, use a separate `Arc` field that the plugin and editor both reference, passed through the `custom_editor()` closure.

## Hover Interaction

Iced's Canvas `Program` receives a `Cursor` in `draw()`:

```rust
fn draw(&self, _state: &(), ..., cursor: mouse::Cursor) -> Vec<Geometry> {
    if let Some(pos) = cursor.position_in(bounds) {
        let freq = x_to_freq(pos.x, bounds.width);
        let bin = (freq / bin_width).round() as usize;
        let db = bins.get(bin).copied().unwrap_or(DB_FLOOR);

        // Draw crosshair and label at cursor position
        let label = format!("{:.0} Hz  {:.1} dB", freq, db);
        frame.fill_text(canvas::Text {
            content: label,
            position: Point::new(pos.x + 8.0, pos.y - 16.0),
            size: 11.0.into(),
            color: Color::WHITE,
            ..Default::default()
        });
    }
}
```

## Pros and Cons

**Pros:**
- `Canvas` widget provides full path-based 2D rendering (fill, stroke, arcs, bezier)
- Strong typing via Elm architecture — state changes are explicit
- GPU-accelerated (wgpu)
- Layout composition via iced's widget system (header panel + canvas)
- Cursor position available in draw for hover feedback

**Cons:**
- Retained-mode requires explicit cache invalidation for animation
- Subscription/tick mechanism needed for continuous repaint
- More boilerplate than egui (Message enum, update, view separation)
- Getting shared state (SpectrumData) into the IcedPlugin requires workaround
- Canvas `Program` trait is more ceremony than egui's `Painter`

## Estimated Complexity

Medium. The Canvas API is capable but the Elm architecture adds indirection. The tick subscription and cache invalidation pattern is well-documented in iced but adds moving parts compared to immediate-mode approaches.
