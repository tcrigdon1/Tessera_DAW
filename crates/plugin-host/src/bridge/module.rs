//! Loading plug-in binaries: finding the library inside a bundle and calling
//! the platform entry points VST3 modules expect.

#[cfg(unix)]
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use tessera_engine::plugin::PluginFormat;

/// Architecture folder names used inside VST3 bundles.
fn vst3_arch_dirs() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            &["arm64-win", "arm64ec-win", "arm64x-win"]
        } else {
            &["x86_64-win"]
        }
    } else if cfg!(target_arch = "aarch64") {
        &["aarch64-linux"]
    } else {
        &["x86_64-linux"]
    }
}

fn first_file(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files.into_iter().next()
}

/// The shared library to load for a plug-in file or bundle.
pub fn binary_path(format: PluginFormat, path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("{} is not installed on this computer", path.display()));
    }
    if !path.is_dir() {
        return Ok(path.to_path_buf());
    }
    let contents = path.join("Contents");
    if cfg!(target_os = "macos") {
        let stem = path.file_stem().map(|s| s.to_os_string()).unwrap_or_default();
        let named = contents.join("MacOS").join(&stem);
        if named.is_file() {
            return Ok(named);
        }
        return first_file(&contents.join("MacOS")).ok_or_else(|| format!("{} has no binary", path.display()));
    }
    if format == PluginFormat::Clap {
        // Some Linux packages wrap the library in a same-named folder.
        if let Some(name) = path.file_name() {
            let inner = path.join(name);
            if inner.is_file() {
                return Ok(inner);
            }
        }
        return Err(format!("{} is a folder, not a CLAP plug-in", path.display()));
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = if cfg!(target_os = "windows") { "vst3" } else { "so" };
    for arch in vst3_arch_dirs() {
        let dir = contents.join(arch);
        let named = dir.join(format!("{stem}.{ext}"));
        if named.is_file() {
            return Ok(named);
        }
        if let Some(f) = std::fs::read_dir(&dir).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().map(|e| e == ext).unwrap_or(false))
        }) {
            return Ok(f);
        }
    }
    Err(format!(
        "{} has no binary for this computer ({})",
        path.display(),
        vst3_arch_dirs().join(", ")
    ))
}

#[cfg(target_os = "macos")]
mod cf {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFURLCreateFromFileSystemRepresentation(
            alloc: *const c_void,
            buf: *const u8,
            len: isize,
            is_directory: u8,
        ) -> *const c_void;
        pub fn CFBundleCreate(alloc: *const c_void, url: *const c_void) -> *const c_void;
        pub fn CFRelease(cf: *const c_void);
    }
}

/// A loaded plug-in library. Dropping it runs the module's exit function
/// and unloads it.
pub struct Module {
    lib: Option<libloading::Library>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    format: PluginFormat,
    entered: bool,
    #[cfg(target_os = "macos")]
    bundle: *const c_void,
    pub path: PathBuf,
}

impl Module {
    pub fn load(format: PluginFormat, path: &Path) -> Result<Module, String> {
        let bin = binary_path(format, path)?;
        let lib = open(&bin).map_err(|e| format!("could not load {}: {e}", bin.display()))?;
        let mut m = Module {
            lib: Some(lib),
            format,
            entered: false,
            #[cfg(target_os = "macos")]
            bundle: std::ptr::null(),
            path: path.to_path_buf(),
        };
        if format == PluginFormat::Vst3 {
            m.enter()?;
        }
        Ok(m)
    }

    pub fn lib(&self) -> &libloading::Library {
        self.lib.as_ref().expect("module is loaded")
    }

    /// Looks up a function or data symbol.
    ///
    /// # Safety
    /// `T` must match the symbol's real type.
    pub unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> Option<T> {
        self.lib().get::<T>(name).ok().map(|s| *s)
    }

    /// VST3 module entry: `ModuleEntry` (Linux), `bundleEntry` (macOS),
    /// `InitDll` (Windows). Missing entry points are fine on Windows only.
    fn enter(&mut self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        unsafe {
            type Entry = unsafe extern "C" fn(*mut c_void) -> bool;
            if let Some(f) = self.symbol::<Entry>(b"ModuleEntry\0") {
                // The SDK passes the module's own handle; plug-ins use it to
                // find their bundle. Re-open the library to get it.
                let bin = binary_path(self.format, &self.path)?;
                let c = std::ffi::CString::new(bin.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
                let h = libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
                if !f(h) {
                    if !h.is_null() {
                        libc::dlclose(h);
                    }
                    return Err("the plug-in's ModuleEntry failed".into());
                }
                if !h.is_null() {
                    libc::dlclose(h);
                }
                self.entered = true;
            }
        }
        #[cfg(target_os = "macos")]
        unsafe {
            type Entry = unsafe extern "C" fn(*const c_void) -> bool;
            if let Some(f) = self.symbol::<Entry>(b"bundleEntry\0") {
                let p = self.path.to_string_lossy().to_string();
                let url =
                    cf::CFURLCreateFromFileSystemRepresentation(std::ptr::null(), p.as_ptr(), p.len() as isize, 1);
                let bundle = if url.is_null() {
                    std::ptr::null()
                } else {
                    let b = cf::CFBundleCreate(std::ptr::null(), url);
                    cf::CFRelease(url);
                    b
                };
                self.bundle = bundle;
                if !f(bundle) {
                    return Err("the plug-in's bundleEntry failed".into());
                }
                self.entered = true;
            }
        }
        #[cfg(target_os = "windows")]
        unsafe {
            type Entry = unsafe extern "system" fn() -> bool;
            if let Some(f) = self.symbol::<Entry>(b"InitDll\0") {
                if !f() {
                    return Err("the plug-in's InitDll failed".into());
                }
                self.entered = true;
            }
        }
        Ok(())
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        if self.entered {
            unsafe {
                #[cfg(target_os = "linux")]
                if let Some(f) = self.symbol::<unsafe extern "C" fn() -> bool>(b"ModuleExit\0") {
                    f();
                }
                #[cfg(target_os = "macos")]
                if let Some(f) = self.symbol::<unsafe extern "C" fn() -> bool>(b"bundleExit\0") {
                    f();
                }
                #[cfg(target_os = "windows")]
                if let Some(f) = self.symbol::<unsafe extern "system" fn() -> bool>(b"ExitDll\0") {
                    f();
                }
            }
        }
        #[cfg(target_os = "macos")]
        if !self.bundle.is_null() {
            unsafe { cf::CFRelease(self.bundle) };
        }
        // Some plug-ins misbehave when unloaded (static destructors racing
        // their own threads); the bridge exits right after this anyway.
        if let Some(lib) = self.lib.take() {
            std::mem::forget(lib);
        }
    }
}

fn open(path: &Path) -> Result<libloading::Library, libloading::Error> {
    #[cfg(unix)]
    unsafe {
        // Local symbols keep plug-ins from interposing on each other.
        libloading::os::unix::Library::open(Some(path), libc::RTLD_NOW | libc::RTLD_LOCAL).map(Into::into)
    }
    #[cfg(windows)]
    unsafe {
        libloading::Library::new(path)
    }
}
