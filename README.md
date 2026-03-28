# Truce Analyzer

A real-time frequency spectrum analyzer audio plugin built with [truce](https://github.com/truce-audio/truce).

Uses a **Constant-Q Transform (CQT)** for logarithmically-spaced frequency resolution — each bin has bandwidth proportional to its center frequency, matching how we perceive pitch. The GUI is rendered with [egui](https://github.com/emilk/egui).

## Features

- CQT-based analysis with 48 bins per octave (27.5 Hz – 20.48 kHz)
- Sparse frequency-domain kernels (Brown-Puckette method) for efficient real-time computation
- Lock-free audio→GUI data transfer via atomics
- Logarithmic frequency axis, dB amplitude axis
- Filled spectrum curve with hover crosshair showing frequency and amplitude
- Pass-through audio with adjustable gain

## Plugin Formats

Builds to CLAP and VST3 by default. AU, VST2, and AAX available via feature flags.

```sh
cargo build --release                           # CLAP + VST3
cargo build --release --features au             # + Audio Units
cargo build --release --features vst2           # + VST2
```

## Development

```sh
cargo build                                     # debug build
cargo test                                      # run tests
cargo truce install --dev --release             # install hot-reload shell
cargo watch -x build                            # iterate with hot-reload
```

## Project Structure

```
src/
  lib.rs    — plugin definition, egui UI, spectrum rendering
  core.rs   — CQT engine, SpectrumData, coordinate helpers
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
