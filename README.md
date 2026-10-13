# Tessera

Tessera is an original, from-scratch digital audio workstation modelled on the
workflow of Ableton Live Suite: a Session view of launchable clips and scenes,
an Arrangement timeline, a mixer, built-in instruments and (on the roadmap)
effects, warping, automation and a browser. All code in this repository is
original.

![Session view](docs/session-view.png)

![Arrangement view](docs/arrangement-view.png)

> Status: **foundation**. The audio engine, transport, mixer, two built-in
> instruments, Session view and Arrangement view work. See the
> [roadmap](#roadmap) for what comes next.

## Features today

- **Real-time audio engine** written in Rust. The audio thread never allocates,
  frees or locks (enforced by a test with a counting allocator).
- **Transport**: play / stop / continue, tempo (20–999 BPM), time signature,
  start marker, arrangement loop, metronome.
- **Session view**: tracks × scenes grid of clip slots, quantized clip and scene
  launching (none, 1/8 … 4 bars), per-track stop buttons, Stop All Clips,
  optional per-scene tempo, Back to Arrangement.
- **Arrangement view**: bar ruler, clips on a timeline, click to set the
  playhead, loop brace, drag clips to move them (snapped to the grid), zoom.
- **Mixer**: volume, pan (constant-power), mute, solo, arm, peak meters per
  track and on the master.
- **Instruments**: *Poly Synth* (2 oscillators, resonant filter with envelope,
  amp envelope, 16 voices) and *Drum Kit* (synthesized kick, snare, clap, hats,
  toms, rim, crash on General MIDI notes).
- **Clip editing**: a piano roll for MIDI clips (add, delete and move notes) and
  waveform display for audio clips.
- **Audio clips**: drop a WAV file onto a slot of an audio track.
- **Automation**: breakpoint envelopes with curves for volume, pan and every
  instrument parameter, as Arrangement automation lanes and as clip envelopes
  that loop with Session clips. Automation Arm records control moves while
  playing; moving an automated control by hand overrides it until
  *Re-Enable* is clicked. Mixer automation is applied every sample.
- **Warping**: audio clips follow the project tempo through warp markers, with
  *Beats* (transient slicing), *Tones* (granular), *Complex* (WSOLA) and
  *Re-Pitch* modes, clip transpose, and transient detection plus tempo
  estimation for *Auto-Warp*. WAV files of 2 seconds or more are auto-warped
  when imported.
- **Projects**: save and open as JSON (`.tessera`). A demo song loads on start.
- **Offline export** of the arrangement to WAV (`tessera render`).

## Building and running

You need a recent stable Rust toolchain (install from <https://rustup.rs>).

### macOS / Windows

```sh
cargo run --release
```

### Linux

Install the ALSA and windowing development packages first. On Debian/Ubuntu:

```sh
sudo apt install build-essential pkg-config libasound2-dev libxkbcommon-dev \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev
cargo run --release
```

### Command line

```sh
tessera                      # open the app with the demo song
tessera path/to/song.tessera # open a project
tessera --play --arrangement # start playing, showing the Arrangement
tessera render out.wav [beats] [project.tessera]
                             # render the arrangement to a WAV file
```

When no audio output device is available the app keeps running on a silent
"null" device so the UI and transport still work.

### Tests

```sh
cargo test --workspace
```

## Using it

| Key | Action |
| --- | --- |
| Space | Play / Stop |
| Shift + Space | Continue from the stop position |
| Tab | Switch between Session and Arrangement view |
| Shift + Tab | Switch the detail panel between Clip and Device |
| Ctrl/Cmd + S / O / N | Save / Open / New |
| Delete | Delete the selected clip or note |
| A S D F G H J K (W E T Y U) | Play the selected track's instrument |
| Z / X | Octave down / up for the computer keyboard |

- Click a slot's ▶ triangle (or double-click the clip) to launch it; click a
  scene's ▶ on the master column to launch the whole row.
- Double-click an empty MIDI slot to create a clip; the piano roll opens below.
  Right-click a slot or a scene for more options (duplicate, delete, rename,
  scene tempo, *Add to Arrangement*).
- In the piano roll, click an empty cell to add a note, drag on empty space to
  draw a longer note, drag a note to move it, drag its right edge to resize it,
  and right-click or double-click a note to delete it. Click the keys to
  audition pitches.
- In the Arrangement, click the ruler or a lane to move the playhead, drag clips
  to move them (hold Alt to bypass the grid), drag a clip's right edge to
  resize it, double-click an empty MIDI lane to create a clip, and drag or
  double-click the loop brace above the ruler. Ctrl/Cmd + mouse wheel zooms.
- **Automation**: click a track's **A** button in the Arrangement to open its
  automation lanes and pick parameters with *+ Add lane*. In a lane, click to
  add a breakpoint, drag points to move them (Alt bypasses the grid), drag a
  line to move a segment, Alt + drag a line to curve it, and double-click or
  right-click a point to delete it. Clip envelopes are on the clip view's
  *Envelopes* tab. Turn on **Auto** in the transport bar and move a fader or
  knob while playing to record automation (into the playing Session clip, or
  the Arrangement).
- **Warping**: select an audio clip to open the sample editor. Toggle *Warp*,
  choose the mode, set the clip tempo (*Seg. BPM*, :2 / ×2) and transpose, or
  click *Auto-Warp*. Double-click the waveform to add a warp marker (it snaps
  to nearby transients), drag markers to stretch the audio, and double-click
  or right-click a marker to remove it.
- The browser on the left creates tracks from the built-in instruments; the
  *Device* view (Shift + Tab) shows the selected track's instrument controls.

## Architecture

```
crates/
  engine/   tessera-engine: project model, real-time engine, DSP, offline render
    src/model.rs      serialisable project (tracks, clips, scenes, mixer)
    src/engine.rs     audio thread: transport, session/arrangement playback, mixer
    src/automation.rs envelopes, parameter ranges, per-track automation buffers
    src/warp.rs       warp markers and the Beats/Tones/Complex/Re-Pitch modes
    src/analysis.rs   transient detection, tempo estimation, auto-warp
    src/dsp/          Poly Synth, Drum Kit, ADSR, state-variable filter
    src/render.rs     offline export
    src/demo.rs       built-in demo song
  app/      tessera: desktop application (egui + cpal)
    src/audio.rs      audio device output (cpal) and null fallback device
    src/app.rs        application state, commands, keyboard shortcuts
    src/ui/           transport bar, Session view, Arrangement view, mixer,
                      detail view (piano roll, devices), widgets
```

**Why Rust + egui.** A DAW needs a hard real-time audio thread, so the engine is
written in Rust: native speed, no garbage collector, and the type system makes
it practical to keep the audio thread free of locks and allocations. The UI is
built with [egui](https://github.com/emilk/egui), an immediate-mode GUI that
redraws at the display's refresh rate. That suits a DAW (playheads, meters and
clip progress animate constantly, and the timeline, clip grid and piano roll are
custom-drawn) and keeps everything in one language and one native binary on
macOS, Windows and Linux, without a browser layer between the UI and the
engine. Audio I/O uses [cpal](https://github.com/RustAudio/cpal) (CoreAudio,
WASAPI, ALSA; JACK later).

**Engine / UI split.** The UI owns the project document. When its structure
changes it is compiled into an immutable snapshot and sent to the audio thread
through a lock-free ring buffer; the audio thread moves live voice and clip
state from the old snapshot into the new one and hands the old one back to the
UI to free. Small, frequent edits (faders, launches, transport) are individual
commands. The engine publishes playhead, meters and clip states through atomics.

**Time.** All positions are stored in beats so projects survive tempo changes.
Session clips follow a monotonic session clock (so they keep playing straight
through arrangement loop jumps); the arrangement follows the song position.
Events are scheduled sample-accurately inside each audio block.

## Roadmap

Planned follow-up work, roughly in order:

1. **Recording**: MIDI input (midir), audio input recording, overdub, session
   record into empty slots, capture MIDI, record Session performance into the
   Arrangement.
2. **Effects and device chains**: per-track device chain with EQ Eight-style
   parametric EQ, compressor, glue-style bus compressor, reverb, delay, chorus,
   saturator, auto filter, utility, limiter; return tracks with sends; racks
   with macros.
3. **Automation** (lanes, clip envelopes, recording and overrides are done):
   send and effect-parameter lanes once the device chain lands, parameter
   modulation, automation in Session-to-Arrangement recording.
4. **Warping and audio editing** (warp markers, four warp modes, transient
   detection and auto-warp are done): a formant-preserving mode, clip start
   offset, reverse, consolidate, audio-to-MIDI.
5. **MIDI editing**: velocity lane, quantize, legato, duplicate/loop
   selection, scales, MIDI effects (arpeggiator, chord, scale, note echo),
   MPE.
6. **More instruments**: wavetable synth, sampler with multisample zones,
   drum rack with sample pads, FM synth, physical-model-style instruments.
7. **Browser**: categories (instruments, effects, samples, clips), preview,
   drag and drop, user library, search, file formats beyond WAV (FLAC, MP3,
   AIFF, OGG).
8. **Plugins**: CLAP and VST3 hosting.
9. **Project features**: undo/redo, collect-all-and-save, templates, tempo and
   time-signature automation, groups, freeze/flatten, track delay.
10. **Performance and I/O**: multi-core track processing, JACK/ASIO,
    plugin delay compensation, Link-style tempo sync, MIDI controller mapping.
11. **Export**: stems, sample-rate/bit-depth options, MP3/FLAC export.

## License

MIT
