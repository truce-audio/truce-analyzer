# Truce Analyzer

**[Download the latest release](https://github.com/truce-audio/truce-analyzer/releases/latest)** — `.pkg` for macOS, `.exe` for Windows, `.tar.gz` for Linux. Ships CLAP, VST3, LV2, and AU v2 (macOS). AU v3 and AAX are opt-in per build (Developer ID signing and PACE/iLok signing respectively).

A real-time frequency spectrum analyzer plugin for debugging/reverse-engineering audio plugins. 

![](screenshots/analyzer_spectrum_macos.png)

Compare signals across your chain without needing additional tracks or sends. Insert one instance before your processing and one after, then select the "before" instance as a source in the "after" instance.

![Before/after EQ comparison showing spectral diff](screenshots/analyzer_diff_macos.png)

- **Red** = boost (your processing added energy)
- **Green** = cut (your processing removed energy)
- **Gray** = the source signal overlaid for reference

Three view modes:
- **Normal** — overlay the source spectrum behind yours
- **Diff** — show only the difference
- **Both** — overlay + diff together (shown above)

You can also select multiple sources to compare against several points in your chain at once.

## Controls

| Control | Location | Description |
|---------|----------|-------------|
| **Instance name** | Header, left | Double-click to rename. Persists across save/load. |
| **Source** | Header, right | Select other instances to compare against. |
| **View** | Header, right | Normal / Diff / Both. Only visible when a source is selected. |
| **Channel** | Header, right | Sum / Both / Left / Right / Diff (M/S side). Hidden when comparing. |
| **Hover** | Spectrum area | Shows frequency, amplitude per signal, and diff at cursor. |

## Formats

Available as CLAP, VST3, LV2, and AU v2 (macOS). Works in any DAW that supports these formats. AU v3 and AAX builds are gated behind their respective signing setups and not part of the default release.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
