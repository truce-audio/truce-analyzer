# Truce Analyzer

A real-time frequency spectrum analyzer plugin for music production. Place multiple instances across your signal chain to visually compare and diff the spectral impact of your processing.

**[Download the latest release](https://github.com/truce-audio/truce-analyzer/releases/latest)** — available as CLAP, VST3, VST2, AU, and AAX.

![](screenshots/analyzer_spectrum.png)

## A/B comparisons made easier with a slight abuse of the plugin system

Compare signals across your chain without needing additional tracks or sends. Insert one instance before your processing and one after, then select the "before" instance as a source in the "after" instance.

![Before/after EQ comparison showing spectral diff](screenshots/analyzer_diff.png)

- **Red** = boost (your processing added energy)
- **Green** = cut (your processing removed energy)
- **Gray** = the source signal overlaid for reference

Three view modes:
- **Normal** — overlay the source spectrum behind yours
- **Diff** — show only the difference
- **Both** — overlay + diff together (shown above)

### Getting Started with A/B

1. Insert Truce Analyzer before your plugin chain
2. Double-click the instance name and rename it (e.g., "Before EQ")
3. Insert another Truce Analyzer after your chain
4. In the second instance, open the **Source** dropdown and select "Before EQ"
5. The spectral difference appears immediately

You can select multiple sources to compare against several points in your chain at once.

## Controls

| Control | Location | Description |
|---------|----------|-------------|
| **Instance name** | Header, left | Double-click to rename. Persists across save/load. |
| **Source** | Header, right | Select other instances to compare against. |
| **View** | Header, right | Normal / Diff / Both. Only visible when a source is selected. |
| **Channel** | Header, right | Sum / Both / Left / Right / Diff (M/S side). Hidden when comparing. |
| **Hover** | Spectrum area | Shows frequency, amplitude per signal, and diff at cursor. |

## Formats

Available as CLAP, VST3, VST2, AU, and AAX. Works in any DAW that supports these formats.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
