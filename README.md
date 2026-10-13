# Tessera

Tessera is an original, from-scratch digital audio workstation modelled on the
workflow of Ableton Live Suite: a Session view of launchable clips and scenes,
an Arrangement timeline, a mixer with return tracks, built-in instruments and
audio effects, audio recording and (on the roadmap) warping, automation and a
browser. All code in this repository is
original.

![Session view](docs/session-view.png)

![Arrangement view](docs/arrangement-view.png)

> Status: **early**. The audio engine, transport, mixer with sends and
> returns, two built-in instruments, eight audio effects, audio recording,
> Session view and Arrangement view work. See the [roadmap](#roadmap) for what
> comes next.

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
  track and on the master. **Return tracks** with post-fader **sends** (new
  projects start with *A Reverb* and *B Delay*).
- **Device chains**: every track, return track and the master has a chain of
  insert effects shown in the Device view after the instrument. Devices can
  be switched off, reordered, reset and deleted; parameter changes apply
  instantly and reverb/delay tails survive edits.
- **Audio effects**: *Parametric EQ* (8 bands: 12/48 dB cuts, shelves, bells,
  notch, with an interactive response curve), *Compressor* (soft knee,
  makeup, dry/wet, gain reduction meter), *Saturator* (soft clip, hard clip,
  tube, fold), *Chorus*, *Delay* (tempo sync, ping-pong, filtered feedback),
  *Reverb* (feedback delay network with pre-delay, decay, damping, width),
  *Limiter* (look-ahead brickwall, on the master by default) and *Utility*
  (gain, width, mono, phase).
- **Audio recording**: arm an audio track to monitor its input (Auto / In /
  Off, input 1/2, 1 or 2). Click the record button in an empty Session slot
  to record into it from the next launch boundary; launch the slot to stop
  and loop the new clip. *Arrangement Record* (F9) records armed tracks into
  the Arrangement from the playhead, with an optional count-in of 1, 2 or 4
  bars, punch in/out and a new take on every loop pass. Input latency is
  compensated; takes are saved as 32-bit float WAVs and collected into
  `Samples/Recorded` next to the project when it is saved.
- **Instruments**: *Poly Synth* (2 oscillators, resonant filter with envelope,
  amp envelope, 16 voices) and *Drum Kit* (synthesized kick, snare, clap, hats,
  toms, rim, crash on General MIDI notes).
- **Clip editing**: a piano roll for MIDI clips (add, delete and move notes) and
  waveform display for audio clips.
- **Audio clips**: drop a WAV file onto a slot of an audio track.
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
| F9 | Arrangement Record |
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
- The browser on the left creates tracks from the built-in instruments and
  adds audio effects to the selected track. The *Device* view (Shift + Tab)
  shows the selected track's instrument and effects; click a return track's
  or the master's header to see theirs. In the EQ, drag a numbered point to
  move a band, scroll over it to change its Q and double-click to switch it
  on or off.
- Send knobs (A, B, …) sit above each track's pan knob. Right-click the
  master or a return header to add or delete return tracks.

## Architecture

```
crates/
  engine/   tessera-engine: project model, real-time engine, DSP, offline render
    src/model.rs      serialisable project (tracks, clips, scenes, mixer)
    src/engine.rs     audio thread: transport, session/arrangement playback, mixer
    src/engine/buses.rs   return tracks and sends
    src/engine/record.rs  audio input, monitoring, recording takes, count-in
    src/dsp/          Poly Synth, Drum Kit, ADSR, state-variable filter
    src/fx/           audio effects and device chains (FxChain)
    src/render.rs     offline export
    src/demo.rs       built-in demo song
  app/      tessera: desktop application (egui + cpal)
    src/audio.rs      audio device input/output (cpal) and null fallback device
    src/app.rs        application state, commands, keyboard shortcuts
    src/devices.rs    editing device chains, sends and return tracks
    src/recording.rs  turns recorded input into clips and WAV files
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

**Effects.** Each effect's settings are a plain `Copy` struct of `f32`
parameters described by a static table (name, range, scale, unit), so the
Device view draws generic controls and single parameters can be set by index
(the hook automation will use). DSP state (delay lines, envelopes) is
allocated on the UI thread when a snapshot is compiled and moved from the old
snapshot to the new one by device id.

**Recording.** The input device writes into a lock-free ring the engine reads
each block. While anything records, the engine streams the raw input to the
UI through a second ring together with sample-accurate start/stop events, so
the audio thread never grows a buffer. The UI cuts the takes out of that
stream, drops the measured input + output latency from the front, writes the
WAV files and adds the clips.

**Time.** All positions are stored in beats so projects survive tempo changes.
Session clips follow a monotonic session clock (so they keep playing straight
through arrangement loop jumps); the arrangement follows the song position.
Events are scheduled sample-accurately inside each audio block.

## Roadmap

Planned follow-up work, roughly in order:

1. **Recording** (audio recording, count-in and Session slot recording are
   done): MIDI input (midir), MIDI overdub, capture MIDI, input device
   selection, take lanes and comping, record Session performance into the
   Arrangement.
2. **Effects and device chains** (EQ, compressor, reverb, delay, chorus,
   saturator, limiter, utility, returns and sends are done): glue-style bus
   compressor, auto filter, gate, sidechain inputs, pre-fader sends, drag and
   drop reordering, racks with macros, plugin delay compensation.
3. **Automation**: arrangement automation lanes, session clip envelopes,
   breakpoint editing, parameter modulation.
4. **Warping and audio editing**: time-stretching (beats/tones/complex style
   modes), warp markers, transient detection, tempo-following audio clips,
   clip transpose, reverse, consolidate, audio-to-MIDI.
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
