# Changelog

All notable changes to Tessera are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.0] - Unreleased

The first public release. Everything below was built in pull requests #1 to
#22.

### Engine and core

- Lock-free real-time audio engine in Rust: the audio thread never
  allocates, frees or locks, enforced by a test (#1).
- Transport with tempo, time signature, metronome, loop and start marker;
  Session view of clips and scenes with quantized launching; Arrangement
  timeline; mixer with volume, pan, mute, solo, arm and meters (#1).
- Notes sit on exact sample positions, including notes that land on the edge
  of an audio buffer (#21).

### Recording, effects and mixing

- Audio recording, return tracks and post-fader sends, and device chains on
  every track, return and the master (#2).
- 29 audio effects across dynamics, EQ, distortion, modulation, delay,
  reverb, pitch and utilities, plus Audio Effect, Instrument, MIDI Effect and
  Drum Racks with macros (#2, #8).
- Audio input at any sample rate, and plug-in windows that open
  automatically when a plug-in is added (#12).
- Master and Return device chains open when you click their title bars, with
  a Reverb, Delay and Master strip above the Arrangement tempo lane (#22).

### Automation and warping

- Automation lanes, clip envelopes and automation recording (#3).
- Audio clip warping with warp markers, four warp modes, transient detection
  and auto-warp (#3).
- Audio to MIDI conversion (Melody, Harmony, Drums) running in the
  background, and Slice to New MIDI Track (#16).

### Instruments, MIDI and the Browser

- Sampler, Drum Rack, Wavetable and FM Synth, with more instruments added
  later, and a Browser with categories, places, search, preview and drag and
  drop (#4, #8).
- Piano roll editing tools, clip launch modes, Follow Actions, MIDI effects
  and MIDI controller input (#6).
- View menu checklist, Overview strip, Note Expression editor, Quantize
  settings (Ctrl+Shift+U) and Record Quantization (#18).
- Clips in the Browser with 58 original factory MIDI clips, Save Chain as
  Template and a Default Track template (#17).
- Device title bar close button and right-click menu, device presets, and
  Quantize on right-click (#20).
- Tuplet grid and tuplet-aware Quantize, per-clip meter (polymeter), and a
  Polyrhythm window with stacked presets, Euclidean thinning and metric
  modulation (#21).

### Arrangement and workflow

- Undo history, groups, freeze and flatten, locators, tempo automation,
  take lanes and comping, and the Groove Pool (#7).
- A second window so Session and Arrangement can sit on separate monitors
  (#15).
- Pitch bend, synced LFOs and other polish (#10); plain `cargo run` starts
  the app (#11).

### Plug-ins and audio drivers

- VST3 and CLAP plug-in hosting, each plug-in in its own sandboxed bridge
  process, with plug-in editors (#5).
- Driver Type choice on Windows: ASIO (optional build feature), WASAPI
  Shared and WASAPI Exclusive, with a buffer and latency readout (#13).

### Files, export and preferences

- Export Audio dialog with stems, WAV, AIFF, FLAC and MP3 (Tessera's own MP3
  encoder), MIDI file import and export, Preferences, light and dark themes,
  autosave and recovery, Info View and help (#9).
- Tessera icon on both windows and the Windows executable, and Windows
  scripts that create "Tessera" and "Update Tessera" shortcuts (#19).

### Project

- CI runs format, clippy and tests on Linux for every pull request, with
  Windows, macOS and ASIO builds on demand (#14).
- Open-source housekeeping: contributing guide, code of conduct, issue
  templates, cargo-deny licence and advisory checks, and this changelog.

[0.1.0]: https://github.com/tcrigdon1/Ableton_Clone
