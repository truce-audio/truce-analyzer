# GUI Framework Comparison for Truce Analyzer

## Context

The truce analyzer is a full-window, real-time frequency spectrum visualizer. The plugin's UI is dominated by a single animated canvas — a filled spectrum curve over a logarithmic frequency grid, updating at ~60fps. There are no knobs, sliders, or complex widget layouts in v0.1; the UI is almost entirely custom drawing.

This workload profile — continuous animation, custom 2D rendering, minimal standard widgets — strongly favors some frameworks over others.

## Summary Table

| Dimension               | Builtin             | egui                  | Iced                  | Slint                   |
|-------------------------|---------------------|-----------------------|-----------------------|-------------------------|
| **Rendering model**     | Primitive calls     | Immediate-mode canvas | Retained canvas+cache | Pixel buffer bridge     |
| **Filled polygon**      | No (vertical strips)| Yes (native)          | Yes (path fill)       | Yes (in pixel buffer)   |
| **Polyline stroke**     | `draw_line` pairs   | `Shape::line`         | `path::Builder`       | In pixel buffer         |
| **Continuous repaint**  | Automatic (idle)    | `request_repaint()`   | Tick subscription     | Automatic (idle sync)   |
| **Hover tooltip**       | Manual hit-test     | 3 lines of code       | Cursor in draw()      | TouchArea + re-render   |
| **GPU required**        | No (CPU default)    | Yes (wgpu)            | Yes (wgpu)            | No (software)           |
| **Hot-reload**          | Yes (render in PluginLogic) | No (custom_editor) | No (custom_editor) | No (custom_editor)      |
| **Binary size impact**  | Minimal             | Large (egui + wgpu)   | Large (iced + wgpu)   | Medium (slint)          |
| **Boilerplate**         | Low                 | Low                   | Medium-high           | Medium-high             |
| **Complexity estimate** | Low-medium          | Low                   | Medium                | Medium-high             |

## Rendering Fit

The core question: *how naturally does each framework support drawing a filled, animated polyline curve?*

### egui — Best fit

egui's `Painter` API was designed for exactly this. A filled spectrum is one call:

```rust
painter.add(Shape::convex_polygon(points, fill_color, Stroke::NONE));
painter.add(Shape::line(curve_points, stroke));
```

No workarounds, no abstractions — the API surface maps 1:1 to what we need. The immediate-mode model also means the entire UI is just a function that runs every frame, which is exactly right for an always-animating visualizer. There is no cache to invalidate, no message to dispatch — just draw.

### Iced — Good fit, more ceremony

Iced's `Canvas` widget with `path::Builder` provides the same path-based fill/stroke capability. The drawing code is comparable to egui. But the Elm architecture adds structural overhead:

- A `Message` enum and `update()` function for what is functionally a stateless draw loop
- A `Subscription` or manual tick to drive animation (iced won't redraw without a reason)
- Canvas `Cache` that must be explicitly cleared each frame
- The `Program` trait impl is more ceremony than egui's closure

For a visualization-heavy plugin with minimal interaction, this architecture provides structure without much payoff.

### Builtin — Capable but limited primitives

The `RenderBackend` trait has `draw_line` and `fill_rect` but no `fill_polygon`. The spectrum stroke is straightforward (chain of `draw_line` calls), but the filled area under the curve requires a workaround: drawing ~700 vertical 1px `fill_rect` strips per frame. This works and performs fine, but it's a hack — the rendering code is less clear than the polygon-based approaches.

The major advantage is zero external dependencies. The `render()` method is part of `PluginLogic`, so it hot-reloads with the DSP code. For iterating on the visualization during development, this is valuable.

### Slint — Poor fit for this workload

Slint's declarative markup is designed for static layouts with property-bound widgets, not data-driven animated polylines. The practical implementation requires rendering the spectrum to an RGBA pixel buffer in Rust and passing it to Slint as an `Image` property. This means:

- Writing a software rasterizer (or pulling in tiny-skia) for the spectrum
- Allocating and copying a pixel buffer every frame
- Running two rendering systems (Slint for the header, pixel buffer for the visualization)
- Hover interaction requires a round-trip: TouchArea -> property -> Rust re-render

The Slint markup for this plugin would be ~20 lines of boilerplate wrapping a pixel buffer. The framework's strengths (IDE preview, declarative bindings, build-time validation) don't apply to the core visualization.

## Data Flow

All four variants share the same lock-free triple-buffer architecture. The differences are in how the GUI side accesses `SpectrumData`:

| Framework | How spectrum data reaches the GUI |
|-----------|----------------------------------|
| Builtin   | `&self` in `render()` — direct field access on plugin struct |
| egui      | `Arc<SpectrumData>` captured in `EguiEditor` closure |
| Iced      | Embedded in params struct or threaded through `IcedPlugin::new()` |
| Slint     | `Arc<SpectrumData>` captured in `SlintEditor` closure |

The **builtin** path is simplest — `render()` runs on the same struct that holds the spectrum data. **egui** and **slint** use closure captures, which is clean. **Iced** is the most awkward because `IcedPlugin::new()` only receives params, so the spectrum data must be smuggled through the params struct or a side channel.

## Interaction Ergonomics

For v0.1 the analyzer is display-only, but hover-to-inspect is a likely v0.2 feature. How hard is it to add?

**egui**: Trivial. `response.hover_pos()` gives cursor position, draw a crosshair and text label in the same frame. ~5 lines.

**Iced**: Easy. The `Canvas::Program::draw()` method receives a `Cursor` argument. Check `cursor.position_in(bounds)`, draw text on the frame. ~8 lines, but the cursor state doesn't trigger a cache clear — need to handle that.

**Builtin**: Possible but manual. Implement `hit_test()` and `on_mouse_moved()` on the plugin, store cursor position, draw in next `render()` call. ~20 lines across two methods.

**Slint**: Awkward. Add a `TouchArea` in markup, bind `mouse-x`/`mouse-y` to properties, read them in Rust, render crosshair into the pixel buffer. The tooltip appears one frame late (read position -> re-render -> display). ~15 lines split across `.slint` and Rust.

## Future Extensibility

If the analyzer grows to include controls (FFT size selector, channel mode toggle, smoothing speed knob) alongside the visualization:

**egui**: Add widgets in the top panel. Immediate-mode makes mixing controls and visualization natural.

**Iced**: The Elm architecture pays off here — controls generate messages, update modifies state, view recomposes. This is what iced was designed for.

**Builtin**: Controls must use `GridLayout` or be drawn manually. Mixing layout-based widgets with custom rendering is possible but the layout becomes split.

**Slint**: This is where Slint shines. Declarative markup for controls alongside the pixel buffer visualization. The `.slint` file becomes the layout authority, and the pixel buffer handles only the spectrum area.

## Build and Binary Impact

| Framework | Additional deps | Approx binary size impact |
|-----------|----------------|---------------------------|
| Builtin   | None           | ~0 KB (already in truce)  |
| egui      | egui, egui-wgpu, wgpu | ~2-4 MB               |
| Iced      | iced, wgpu, winit | ~2-4 MB                 |
| Slint     | slint, slint-build | ~1-2 MB                |

The GPU-backed frameworks (egui, iced) pull in the wgpu stack, which is the largest contributor. Slint uses software rendering by default and avoids wgpu. Builtin adds nothing beyond what truce already includes.

## Hot-Reload

Only the **builtin** variant benefits from truce's hot-reload. The `render()` method is part of `PluginLogic`, which lives in the hot-reloadable dylib. Change the rendering code, `cargo build`, and the plugin updates live in the DAW.

The other three frameworks return editors from `custom_editor()`, which creates a `Box<dyn Editor>`. This editor object is constructed once when the GUI opens and is not part of the hot-reload boundary. Changing the visualization code requires closing and reopening the plugin window.

For rapid visual iteration during development, this gives builtin a significant workflow advantage.

## Recommendation

**For this project (visualization-dominant, minimal controls):**

| Rank | Framework | Rationale |
|------|-----------|-----------|
| 1    | **egui**  | Best rendering API for the task. Polygon fill, polyline stroke, hover interaction, and continuous repaint are all native. Lowest implementation complexity. |
| 2    | **Builtin** | Zero dependencies, hot-reload support, smallest binary. The fill_rect workaround for filled polygons is the only rough edge. Best for rapid iteration. |
| 3    | **Iced**  | Capable Canvas API but the Elm architecture adds friction without much benefit for a display-only visualizer. Better choice if the plugin grows significant controls. |
| 4    | **Slint** | Wrong tool for this job. The pixel-buffer bridge negates Slint's advantages. Would rank higher for a control-panel-heavy plugin with a small visualization inset. |

The ranking would shift if the plugin evolves toward a complex control surface with the spectrum as one component among many — iced and slint would move up, builtin would move down.
