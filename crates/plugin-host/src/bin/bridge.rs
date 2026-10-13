//! The plug-in bridge as a standalone program (Tessera itself also acts as
//! one when started with `--plugin-bridge`).

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(tessera_plugin_host::bridge::main(&args));
}
