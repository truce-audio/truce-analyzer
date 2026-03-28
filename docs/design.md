# Truce Analyzer: Real-Time Frequency Spectrum Visualizer

## Overview

Truce Analyzer is an audio effect plugin that performs real-time FFT-based frequency analysis and renders a live amplitude spectrum. The plugin passes audio through unmodified (or with optional gain) while computing and displaying the frequency content of the input signal.

The plugin ships in four GUI variants — builtin, egui, iced, and slint — each documented separately. This document covers the shared DSP core and architecture.

## Goals

- Real-time spectrum display showing amplitude (dB) vs frequency (Hz) for the input signal
- Smooth, responsive visualization at ~60 fps with no audio thread stalls
- Stereo input: display left/right independently or summed to mono
- Logarithmic frequency axis (perceptually uniform) and dB amplitude axis
- Pass-through audio (the plugin is an analyzer, not a processor)

## Architecture

```
Audio Thread                          GUI Thread (~60fps)
============                          ===================

  input samples                         read spectrum_data
       |                                      |
  accumulate into                     lock-free AtomicF32 array
  ring buffer                                 |
       |                               map bins -> screen coords
  when buffer full:                          |
    apply window -> FFT               render spectrum curve
    compute magnitudes                      |
    write to shared spectrum          draw axis labels / grid
       |
  pass-through output
```

### Thread Communication

The audio thread and GUI thread must never block each other. The spectrum data flows from audio to GUI through a **lock-free mechanism**:

**Option A — Triple buffer (recommended)**
Use a triple-buffer crate (e.g., `triple_buffer`) so the audio thread always has a buffer to write into and the GUI thread always has the most recent complete frame to read. Zero contention, zero allocation after init.

**Option B — Atomic array**
A fixed-size `[AtomicU32; NUM_BINS]` array where the audio thread writes `f32` magnitudes via `f32::to_bits()` / `AtomicU32::store(Relaxed)` and the GUI reads via `load(Relaxed)` / `f32::from_bits()`. Simple and effective for a single spectrum snapshot.

**Option C — Ring buffer of frames**
A lock-free SPSC ring buffer (e.g., `ringbuf`) of complete spectrum frames. The GUI pops the latest, skipping stale frames. Good if the GUI wants to interpolate between frames.

Recommendation: **Option A** for simplicity and correctness. Triple-buffer gives exactly-once latest-frame semantics with zero overhead.

## DSP Pipeline

### 1. Ring Buffer Accumulation

The audio callback receives variable-size blocks (typically 64–2048 samples). We accumulate samples into a ring buffer until we have enough for one FFT frame.

```
FFT_SIZE = 4096        (≈93ms at 44.1kHz, ~10.7 Hz bin resolution)
HOP_SIZE = FFT_SIZE/2  (50% overlap for smoother updates)
```

At 44.1kHz with hop size 2048, the spectrum updates ~21 times/second — well above the visual refresh rate.

### 2. Windowing

Before FFT, apply a **Hann window** to reduce spectral leakage:

```rust
fn hann_window(n: usize, total: usize) -> f32 {
    0.5 * (1.0 - (2.0 * std::f32::consts::PI * n as f32 / total as f32).cos())
}
```

Pre-compute the window table once in `reset()`.

### 3. FFT

Use the `realfft` crate (wraps `rustfft`) for real-input FFT. A real-valued FFT of size N produces N/2+1 complex bins.

```rust
// In reset():
let mut planner = RealFftPlanner::<f32>::new();
self.fft = planner.plan_fft_forward(FFT_SIZE);
self.fft_input = self.fft.make_input_vec();    // [f32; FFT_SIZE]
self.fft_output = self.fft.make_output_vec();   // [Complex<f32>; FFT_SIZE/2 + 1]
```

### 4. Magnitude Computation

Convert complex FFT output to dB magnitudes:

```rust
for (i, bin) in fft_output.iter().enumerate() {
    let magnitude = bin.norm() / (FFT_SIZE as f32);  // normalize
    let db = 20.0 * magnitude.max(1e-10).log10();    // to dB, floor at -200dB
    spectrum_db[i] = db;
}
```

### 5. Smoothing

Apply exponential smoothing to avoid jittery visuals:

```rust
const SMOOTH_DOWN: f32 = 0.8;  // slower decay (hold peaks briefly)
const SMOOTH_UP: f32 = 0.4;    // faster attack

for i in 0..num_bins {
    let alpha = if new[i] > current[i] { SMOOTH_UP } else { SMOOTH_DOWN };
    current[i] = current[i] + alpha * (new[i] - current[i]);
}
```

### 6. Write to Shared Buffer

After smoothing, write the final dB values to the shared triple-buffer (or atomic array) for the GUI to consume.

## Parameters

```rust
#[derive(Params)]
pub struct AnalyzerParams {
    #[param(name = "Gain", range = "linear(-60, 6)", unit = "dB", smooth = "exp(5)")]
    pub gain: FloatParam,
}
```

Future parameters (not in v0.1):
- FFT size selector (1024 / 2048 / 4096 / 8192)
- Smoothing speed
- Channel mode (L / R / L+R / stereo overlay)
- dB range (floor / ceiling)
- Hold peaks toggle

## Shared Types

All four plugin variants share a common core. This should live in a shared crate or module:

```
plugins/
  truce-analyzer-core/       <-- NEW shared crate
    src/lib.rs
      AnalyzerCore            struct holding FFT state + ring buffer
      AnalyzerParams          shared parameter definitions
      SpectrumData            the lock-free shared spectrum buffer
      constants               FFT_SIZE, NUM_BINS, DB_FLOOR, etc.
  truce-analyzer-builtin/
  truce-analyzer-egui/
  truce-analyzer-iced/
  truce-analyzer-slint/
```

### AnalyzerCore

```rust
pub struct AnalyzerCore {
    sample_rate: f64,
    ring_buffer: Vec<f32>,       // accumulation buffer
    ring_pos: usize,
    window: Vec<f32>,            // pre-computed Hann window
    fft: Arc<dyn RealToComplex<f32>>,
    fft_input: Vec<f32>,
    fft_output: Vec<Complex<f32>>,
    smoothed_spectrum: Vec<f32>, // smoothed dB values
    spectrum_out: TripleBufferSender<Vec<f32>>,  // lock-free send to GUI
}

impl AnalyzerCore {
    pub fn new(spectrum_sender: TripleBufferSender<Vec<f32>>) -> Self;
    pub fn reset(&mut self, sample_rate: f64);
    pub fn process(&mut self, input: &[f32]);  // called per-channel from audio thread
}
```

### SpectrumData (GUI side)

```rust
pub struct SpectrumData {
    receiver: TripleBufferReceiver<Vec<f32>>,  // lock-free receive from audio
    num_bins: usize,
    sample_rate: f64,
}

impl SpectrumData {
    /// Get the latest spectrum frame. Returns dB values for each FFT bin.
    pub fn read(&mut self) -> &[f32];

    /// Frequency in Hz for a given bin index.
    pub fn bin_frequency(&self, bin: usize) -> f32 {
        bin as f32 * self.sample_rate as f32 / (self.num_bins as f32 * 2.0)
    }
}
```

## Rendering Specification

All four GUI variants render the same logical visualization:

### Coordinate Mapping

**X-axis: Frequency (logarithmic)**
- Range: 20 Hz to 20,000 Hz (Nyquist or 20kHz, whichever is lower)
- Mapping: `x = (log(freq) - log(20)) / (log(20000) - log(20)) * width`
- Grid lines at: 50, 100, 200, 500, 1k, 2k, 5k, 10k, 20k Hz

**Y-axis: Amplitude (dB, linear in dB)**
- Range: -90 dB (floor) to 0 dB (ceiling)
- Mapping: `y = (1.0 - (db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)) * height`
- Grid lines at: 0, -12, -24, -36, -48, -60, -72, -84 dB

### Visual Elements

1. **Background** — dark surface color
2. **Grid lines** — subtle horizontal (dB) and vertical (Hz) lines
3. **Axis labels** — frequency labels along bottom, dB labels along left
4. **Spectrum curve** — filled or stroked polyline from bin 0 to N, mapped through the coordinate system
5. **Header** — plugin name and version

### Colors (matching truce dark theme)

- Background: `#1a1a2e`
- Grid lines: `#2a2a4a`
- Spectrum fill: `#4a90d9` at 40% opacity
- Spectrum stroke: `#6ab0ff` at 2px
- Labels: `#888899`

## Dependencies

Add to workspace `Cargo.toml`:

```toml
realfft = "3"
triple_buffer = "8"
```

## File Layout After Implementation

```
truce-analyzer/
  Cargo.toml
  truce.toml
  docs/
    design.md                  (this file)
    design-builtin.md
    design-egui.md
    design-iced.md
    design-slint.md
  plugins/
    truce-analyzer-core/       shared DSP + types
      Cargo.toml
      src/lib.rs
    truce-analyzer-builtin/    builtin RenderBackend visualization
    truce-analyzer-egui/       egui immediate-mode visualization
    truce-analyzer-iced/       iced retained-mode visualization
    truce-analyzer-slint/      slint declarative visualization
```

## Open Questions

1. **Stereo handling**: Sum L+R to mono for a single curve, or overlay two curves? Start with mono sum for simplicity.
2. **Peak hold**: Show slowly-decaying peak markers above the live spectrum? Defer to v0.2.
3. **GPU acceleration**: The builtin backend supports both CPU (tiny-skia) and GPU (wgpu). CPU is sufficient for line rendering at this scale. Revisit if performance is an issue.
4. **Shared crate or shared module**: A workspace crate (`truce-analyzer-core`) keeps things clean. Alternatively, a `shared/` module with path dependencies works too.
