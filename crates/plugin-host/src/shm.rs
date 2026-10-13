//! The shared-memory block that carries audio, notes and parameter changes
//! between Tessera's audio thread and a bridge's audio thread.
//!
//! One block exists per plug-in instance. Tessera writes a request (input
//! audio, events, transport), bumps [`Block::request`] and signals the
//! bridge; the bridge processes, writes the output, sets [`Block::done`] to
//! the request number and signals back. Only one side touches the payload at
//! a time, decided by those two counters.

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

use tessera_engine::MAX_BLOCK;

pub const MAGIC: u32 = 0x5453_4231;
pub const VERSION: u32 = 1;
pub const MAX_FRAMES: usize = MAX_BLOCK;
pub const MAX_NOTES: usize = 1024;
pub const MAX_PARAM_CHANGES: usize = 1024;

pub const NOTE_ON: u32 = 0;
pub const NOTE_OFF: u32 = 1;
pub const NOTE_ALL_OFF: u32 = 2;

/// Request flag: clear tails and voices before processing.
pub const FLAG_RESET: u32 = 1;
/// Request flag: rendering offline (export).
pub const FLAG_OFFLINE: u32 = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Transport {
    pub tempo: f64,
    pub song_beat: f64,
    /// Song position in samples.
    pub song_samples: i64,
    pub playing: u32,
    pub beats_per_bar: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoteEvent {
    pub frame: u32,
    pub kind: u32,
    pub key: u32,
    pub velocity: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ParamChange {
    pub frame: u32,
    pub index: u32,
    /// Normalised 0..1.
    pub value: f64,
}

#[repr(C)]
pub struct Block {
    pub magic: u32,
    pub version: u32,
    /// Incremented by Tessera for each request.
    pub request: AtomicU32,
    /// Set by the bridge to the request it finished.
    pub done: AtomicU32,
    pub frames: u32,
    pub flags: u32,
    pub n_notes: u32,
    pub n_params: u32,
    /// 1 when the plug-in processed the request.
    pub status: u32,
    pub _pad: u32,
    pub transport: Transport,
    pub notes: [NoteEvent; MAX_NOTES],
    pub params: [ParamChange; MAX_PARAM_CHANGES],
    pub input: [[f32; MAX_FRAMES]; 2],
    pub output: [[f32; MAX_FRAMES]; 2],
}

pub const BLOCK_SIZE: usize = std::mem::size_of::<Block>();

impl Block {
    /// Number of the last request, for comparing with `done`.
    pub fn idle(&self) -> bool {
        self.request.load(Ordering::Acquire) == self.done.load(Ordering::Acquire)
    }
}

/// A mapping of one [`Block`], created by Tessera and opened by the bridge.
pub struct SharedBlock {
    ptr: *mut Block,
    /// Name passed to the bridge (a file path on Unix, a mapping name on
    /// Windows).
    pub name: String,
    #[cfg(unix)]
    _map: memmap2::MmapMut,
    #[cfg(unix)]
    path: std::sync::Mutex<Option<std::path::PathBuf>>,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

unsafe impl Send for SharedBlock {}
unsafe impl Sync for SharedBlock {}

impl SharedBlock {
    pub fn block(&self) -> *mut Block {
        self.ptr
    }

    fn init(&mut self) {
        // Zero bytes are a valid Block (atomics and plain numbers).
        unsafe {
            std::ptr::write_bytes(self.ptr as *mut u8, 0, BLOCK_SIZE);
            (*self.ptr).magic = MAGIC;
            (*self.ptr).version = VERSION;
        }
    }

    fn check(&self) -> io::Result<()> {
        let b = unsafe { &*self.ptr };
        if b.magic != MAGIC || b.version != VERSION {
            return Err(io::Error::other("shared memory has the wrong layout"));
        }
        Ok(())
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::fs::OpenOptions;
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        let shm = PathBuf::from("/dev/shm");
        if shm.is_dir() {
            shm
        } else {
            std::env::temp_dir()
        }
    }

    impl SharedBlock {
        pub fn create(tag: &str) -> io::Result<SharedBlock> {
            let path = dir().join(format!("tessera-{}-{tag}.shm", std::process::id()));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)?;
            file.set_len(BLOCK_SIZE as u64)?;
            let mut map = unsafe { memmap2::MmapMut::map_mut(&file)? };
            let ptr = map.as_mut_ptr() as *mut Block;
            let mut s = SharedBlock {
                ptr,
                name: path.to_string_lossy().to_string(),
                _map: map,
                path: std::sync::Mutex::new(Some(path)),
            };
            s.init();
            Ok(s)
        }

        pub fn open(name: &str) -> io::Result<SharedBlock> {
            let file = OpenOptions::new().read(true).write(true).open(name)?;
            if file.metadata()?.len() < BLOCK_SIZE as u64 {
                return Err(io::Error::other("shared memory is too small"));
            }
            let mut map = unsafe { memmap2::MmapMut::map_mut(&file)? };
            let ptr = map.as_mut_ptr() as *mut Block;
            let s = SharedBlock {
                ptr,
                name: name.to_string(),
                _map: map,
                path: std::sync::Mutex::new(None),
            };
            s.check()?;
            Ok(s)
        }

        /// Removes the backing file once both sides have mapped it.
        pub fn unlink(&self) {
            if let Some(p) = self.path.lock().unwrap_or_else(|e| e.into_inner()).take() {
                std::fs::remove_file(p).ok();
            }
        }
    }

    impl Drop for SharedBlock {
        fn drop(&mut self) {
            self.unlink();
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
        MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
    };

    pub(crate) fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl SharedBlock {
        pub fn create(tag: &str) -> io::Result<SharedBlock> {
            let name = format!("Local\\tessera-{}-{tag}-shm", std::process::id());
            let w = wide(&name);
            unsafe {
                let handle = CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    std::ptr::null(),
                    PAGE_READWRITE,
                    0,
                    BLOCK_SIZE as u32,
                    w.as_ptr(),
                );
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, BLOCK_SIZE);
                if view.Value.is_null() {
                    let e = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(e);
                }
                let mut s = SharedBlock {
                    ptr: view.Value as *mut Block,
                    name,
                    handle,
                };
                s.init();
                Ok(s)
            }
        }

        pub fn open(name: &str) -> io::Result<SharedBlock> {
            let w = wide(name);
            unsafe {
                let handle = OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, w.as_ptr());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, BLOCK_SIZE);
                if view.Value.is_null() {
                    let e = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(e);
                }
                let s = SharedBlock {
                    ptr: view.Value as *mut Block,
                    name: name.to_string(),
                    handle,
                };
                s.check()?;
                Ok(s)
            }
        }

        /// Named mappings disappear with their last handle; nothing to do.
        pub fn unlink(&self) {}
    }

    impl Drop for SharedBlock {
        fn drop(&mut self) {
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.ptr as *mut _,
                });
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use imp::wide;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_see_the_same_memory() {
        let host = SharedBlock::create("test-shm").unwrap();
        let bridge = SharedBlock::open(&host.name).unwrap();
        unsafe {
            (*host.block()).input[0][5] = 0.75;
            (*host.block()).request.store(3, Ordering::Release);
            assert_eq!((*bridge.block()).input[0][5], 0.75);
            assert_eq!((*bridge.block()).request.load(Ordering::Acquire), 3);
            (*bridge.block()).done.store(3, Ordering::Release);
            assert!((*host.block()).idle());
        }
    }
}
