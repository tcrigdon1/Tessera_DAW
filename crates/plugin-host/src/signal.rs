//! A one-way, cross-process wake-up signal.
//!
//! Unix uses a pipe (one byte per signal, `poll` with a timeout to wait), so
//! the waiting side also learns immediately when the other process dies (the
//! pipe reports end-of-file). Windows uses a named auto-reset event; Tessera
//! additionally waits on the bridge's process handle.

use std::io;
use std::time::Duration;

/// Outcome of a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    Signalled,
    TimedOut,
    /// The other process is gone.
    Closed,
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::fd::RawFd;

    /// The sending end.
    pub struct Sender {
        fd: RawFd,
    }

    /// The waiting end.
    pub struct Receiver {
        fd: RawFd,
    }

    unsafe impl Send for Sender {}
    unsafe impl Sync for Sender {}
    unsafe impl Send for Receiver {}
    unsafe impl Sync for Receiver {}

    fn set_flags(fd: RawFd, cloexec: bool, nonblock: bool) {
        unsafe {
            let fdf = libc::fcntl(fd, libc::F_GETFD);
            let fdf = if cloexec {
                fdf | libc::FD_CLOEXEC
            } else {
                fdf & !libc::FD_CLOEXEC
            };
            libc::fcntl(fd, libc::F_SETFD, fdf);
            let fl = libc::fcntl(fd, libc::F_GETFL);
            let fl = if nonblock {
                fl | libc::O_NONBLOCK
            } else {
                fl & !libc::O_NONBLOCK
            };
            libc::fcntl(fd, libc::F_SETFL, fl);
        }
    }

    /// Creates a signal. Both ends are close-on-exec; the end handed to the
    /// bridge is made inheritable in the child only (see
    /// [`crate::client`]).
    pub fn channel() -> io::Result<(Sender, Receiver)> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        set_flags(fds[0], true, true);
        set_flags(fds[1], true, true);
        Ok((Sender { fd: fds[1] }, Receiver { fd: fds[0] }))
    }

    impl Sender {
        pub fn raw(&self) -> RawFd {
            self.fd
        }

        /// Takes over an inherited descriptor.
        ///
        /// # Safety
        /// `fd` must be an open pipe end that nothing else owns.
        pub unsafe fn from_raw(fd: RawFd) -> Sender {
            set_flags(fd, true, true);
            Sender { fd }
        }

        /// Wakes the other side. Never blocks.
        pub fn signal(&self) -> bool {
            let b = 1u8;
            let n = unsafe { libc::write(self.fd, &b as *const u8 as *const libc::c_void, 1) };
            // A full pipe means the other side has wake-ups pending anyway.
            n == 1 || io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN)
        }
    }

    impl Receiver {
        pub fn raw(&self) -> RawFd {
            self.fd
        }

        /// Takes over an inherited descriptor.
        ///
        /// # Safety
        /// `fd` must be an open pipe end that nothing else owns.
        pub unsafe fn from_raw(fd: RawFd) -> Receiver {
            set_flags(fd, true, true);
            Receiver { fd }
        }

        /// Waits up to `timeout` (forever with `None`) for a signal, then
        /// consumes every pending wake-up. Does not allocate.
        pub fn wait(&self, timeout: Option<Duration>) -> Wait {
            let ms = match timeout {
                Some(t) => t.as_millis().min(i32::MAX as u128) as i32,
                None => -1,
            };
            let mut pfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            loop {
                let r = unsafe { libc::poll(&mut pfd, 1, ms) };
                if r < 0 {
                    if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    return Wait::Closed;
                }
                if r == 0 {
                    return Wait::TimedOut;
                }
                break;
            }
            let mut buf = [0u8; 64];
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                Wait::Signalled
            } else if n == 0 {
                Wait::Closed
            } else if io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
                // Woken by a hang-up with data already consumed elsewhere.
                if pfd.revents & libc::POLLHUP != 0 {
                    Wait::Closed
                } else {
                    Wait::TimedOut
                }
            } else {
                Wait::Closed
            }
        }
    }

    impl Drop for Sender {
        fn drop(&mut self) {
            unsafe { libc::close(self.fd) };
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            unsafe { libc::close(self.fd) };
        }
    }

    /// Makes a descriptor survive `exec` (call in the child between fork and
    /// exec).
    pub fn inherit(fd: RawFd) {
        unsafe {
            let fdf = libc::fcntl(fd, libc::F_GETFD);
            libc::fcntl(fd, libc::F_SETFD, fdf & !libc::FD_CLOEXEC);
            // The bridge waits blocking on its end.
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl & !libc::O_NONBLOCK);
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, OpenEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE,
        SYNCHRONIZATION_SYNCHRONIZE,
    };

    use crate::shm::wide;

    struct Event(HANDLE);
    unsafe impl Send for Event {}
    unsafe impl Sync for Event {}
    impl Drop for Event {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub struct Sender {
        ev: Event,
        pub name: String,
    }

    pub struct Receiver {
        ev: Event,
        pub name: String,
        /// Process whose exit also ends a wait (Tessera's view of a bridge).
        process: AtomicUsize,
    }

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub fn channel() -> io::Result<(Sender, Receiver)> {
        let name = format!(
            "Local\\tessera-{}-{}-sig",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let w = wide(&name);
        let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, w.as_ptr()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let h2 = unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE, 0, w.as_ptr()) };
        if h2.is_null() {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(h) };
            return Err(e);
        }
        Ok((
            Sender {
                ev: Event(h),
                name: name.clone(),
            },
            Receiver {
                ev: Event(h2),
                name,
                process: AtomicUsize::new(0),
            },
        ))
    }

    fn open(name: &str) -> io::Result<Event> {
        let w = wide(name);
        let h = unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE, 0, w.as_ptr()) };
        if h.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Event(h))
        }
    }

    impl Sender {
        pub fn open(name: &str) -> io::Result<Sender> {
            Ok(Sender {
                ev: open(name)?,
                name: name.to_string(),
            })
        }

        pub fn signal(&self) -> bool {
            unsafe { SetEvent(self.ev.0) != 0 }
        }
    }

    impl Receiver {
        pub fn open(name: &str) -> io::Result<Receiver> {
            Ok(Receiver {
                ev: open(name)?,
                name: name.to_string(),
                process: AtomicUsize::new(0),
            })
        }

        /// Also return [`Wait::Closed`] when this process handle is signalled
        /// (the process exited). The handle must stay open while waiting.
        pub fn watch_process(&self, handle: HANDLE) {
            self.process.store(handle as usize, Ordering::Release);
        }

        pub fn wait(&self, timeout: Option<Duration>) -> Wait {
            let ms = match timeout {
                Some(t) => t.as_millis().min((INFINITE - 1) as u128) as u32,
                None => INFINITE,
            };
            let process = self.process.load(Ordering::Acquire) as HANDLE;
            let r = unsafe {
                if process.is_null() {
                    WaitForSingleObject(self.ev.0, ms)
                } else {
                    let handles = [self.ev.0, process];
                    WaitForMultipleObjects(2, handles.as_ptr(), 0, ms)
                }
            };
            if r == WAIT_OBJECT_0 {
                Wait::Signalled
            } else if r == WAIT_TIMEOUT {
                Wait::TimedOut
            } else {
                Wait::Closed
            }
        }
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_wake_and_time_out() {
        let (tx, rx) = channel().unwrap();
        assert_eq!(rx.wait(Some(Duration::from_millis(5))), Wait::TimedOut);
        tx.signal();
        tx.signal();
        assert_eq!(rx.wait(Some(Duration::from_millis(100))), Wait::Signalled);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.signal();
            tx
        });
        assert_eq!(rx.wait(Some(Duration::from_secs(5))), Wait::Signalled);
        let tx = t.join().unwrap();
        drop(tx);
        #[cfg(unix)]
        assert_eq!(rx.wait(Some(Duration::from_millis(100))), Wait::Closed);
    }
}
