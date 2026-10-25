//! A self-pipe, so anything off the main thread can interrupt its `poll`.
//!
//! Three things need to wake the render loop: a front-end sending a request,
//! mpv signalling that a frame is ready, and the window system. The last one
//! has its own file descriptor; the other two share this pipe. Writing one
//! byte to a pipe is the oldest trick in the Unix book and the only one that
//! is safe to call from mpv's own thread.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;

use anyhow::{Context, Result};

#[derive(Clone)]
pub struct Waker {
    write: Arc<OwnedFd>,
}

pub struct WakeReceiver {
    read: OwnedFd,
}

/// Creates a non-blocking pipe pair.
pub fn pipe() -> Result<(Waker, WakeReceiver)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: fds is a two-element array, which is what pipe2 writes into.
    // O_CLOEXEC keeps the descriptors out of anything we spawn, O_NONBLOCK
    // means a full pipe drops the wakeup instead of blocking a writer — and a
    // dropped wakeup is harmless, because a full pipe already means the loop
    // has one pending.
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot create the wakeup pipe");
    }
    // SAFETY: both descriptors are fresh and owned by us.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((
        Waker {
            write: Arc::new(write),
        },
        WakeReceiver { read },
    ))
}

impl Waker {
    /// Nudges the loop. Safe to call from any thread, including mpv's.
    pub fn wake(&self) {
        let byte = 1u8;
        // SAFETY: a one-byte write to a descriptor we own. A failure means the
        // pipe is full, which already implies a pending wakeup, so the result
        // is deliberately ignored rather than retried.
        unsafe {
            libc::write(
                self.write.as_raw_fd(),
                &byte as *const u8 as *const libc::c_void,
                1,
            );
        }
    }

    pub fn raw_fd(&self) -> RawFd {
        self.write.as_raw_fd()
    }
}

impl WakeReceiver {
    pub fn raw_fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Empties the pipe. Wakeups carry no information beyond "look again", so
    /// one drain covers however many bytes arrived.
    pub fn drain(&self) {
        let mut buffer = [0u8; 64];
        loop {
            // SAFETY: reading into a local buffer from a descriptor we own.
            // The read end is non-blocking, so this returns -1/EAGAIN when
            // empty rather than blocking the render loop.
            let read = unsafe {
                libc::read(
                    self.read.as_raw_fd(),
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                )
            };
            if read < buffer.len() as isize {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wakeup_is_readable_and_drains_clean() {
        let (waker, receiver) = pipe().unwrap();
        waker.wake();
        waker.wake();
        receiver.drain();
        // Draining twice must not block, which is the whole point of
        // O_NONBLOCK on the read end.
        receiver.drain();
    }

    #[test]
    fn a_waker_survives_being_cloned_across_threads() {
        let (waker, receiver) = pipe().unwrap();
        let clone = waker.clone();
        let handle = std::thread::spawn(move || clone.wake());
        handle.join().unwrap();
        receiver.drain();
        drop(waker);
    }
}
