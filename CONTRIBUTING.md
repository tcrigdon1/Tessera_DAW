# Contributing to Tessera

Thanks for helping. Tessera is a young project, so bug reports from real use
are just as valuable as code.

## Reporting a bug

Open an issue with the **Bug report** template. The most useful reports say:

- your OS, audio interface and driver type (Preferences → Audio shows it),
- the exact steps, starting from a new project if you can,
- what you expected and what happened,
- the crash log if there was one (Preferences → Licenses/Maintenance →
  Crash Logs → View).

If a project file shows the problem, attach it (File → Collect All and Save
first so the samples come with it), as long as you're happy to share the
audio in it.

## Building

See [Building and running](README.md#building-and-running) in the README.

## Before you open a pull request

Run the same checks CI runs:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you change dependencies, also run
[`cargo deny`](https://github.com/EmbarkStudios/cargo-deny)
(`cargo install cargo-deny --locked`, then `cargo deny check`). It checks
licences and security advisories against [deny.toml](deny.toml).

## Ground rules for code

- **The audio thread never allocates, frees or locks.** The engine talks to the
  UI through lock-free queues; `crates/engine/tests/realtime.rs` fails if the
  audio callback allocates. Keep it passing.
- Keep pull requests focused on one change, with a short description of what
  and why. Screenshots help for UI changes.
- Add a test for engine and DSP changes where you can.
- Write code that reads like the code around it.

## Licensing of contributions

Tessera is MIT licensed. By opening a pull request you agree that your
contribution is released under the same licence.

Only contribute code, samples, presets and other content that you wrote
yourself or that is under a licence compatible with MIT (or CC0 for
content). In particular:

- Do not commit Steinberg's ASIO SDK or any other SDK whose licence doesn't
  allow redistribution under MIT. ASIO stays an optional build feature that
  downloads the SDK at build time.
- Do not add samples, loops, presets or MIDI from commercial packs or other
  DAWs.
- Tessera is inspired by Ableton Live's workflow but is not affiliated with
  Ableton. Don't copy Ableton's artwork, icons, manuals or sounds, and don't use
  its name in ways that suggest an official connection.

## Code of Conduct

Everyone taking part is expected to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
