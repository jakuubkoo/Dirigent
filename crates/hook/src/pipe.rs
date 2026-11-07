//! Windows named pipes (byte mode, local clients only): the app's single-instance endpoint, and later the hook
//! transport. Synchronous: each pipe instance is served by one blocked thread (no wakeups while idle).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};

/// `\\.\pipe\<name>`.
pub fn path(name: &str) -> String {
    format!(r"\\.\pipe\{name}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// One instance of a named pipe. `accept` blocks until a client connects; more instances (more `bind`s of the same
/// name) serve clients concurrently.
#[derive(Debug)]
pub struct Server {
    pipe: File,
}

impl Server {
    /// `first`: fail with `AddrInUse` when the name exists already (another process serves it). Default security:
    /// only the owner (and admins/SYSTEM) can connect for writing.
    pub fn bind(name: &str, first: bool) -> io::Result<Self> {
        let mode = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let kind = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: a NUL-terminated name; null security attributes = default security.
        let h = unsafe {
            CreateNamedPipeW(
                wide(&path(name)).as_ptr(),
                mode,
                kind,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                std::ptr::null(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            if first && e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, e));
            }
            return Err(e);
        }
        // SAFETY: a fresh handle that only we own.
        Ok(Self { pipe: unsafe { File::from_raw_handle(h) } })
    }

    /// Waits for the next client. The instance stays registered all along (a client never sees "not found" while
    /// we run); it is free for the next client once the returned `Conn` drops.
    pub fn accept(&self) -> io::Result<Conn<'_>> {
        // SAFETY: our pipe handle, synchronous mode (no OVERLAPPED).
        if unsafe { ConnectNamedPipe(self.pipe.as_raw_handle(), std::ptr::null_mut()) } == 0 {
            let e = io::Error::last_os_error();
            // A client that connected between CreateNamedPipe and ConnectNamedPipe.
            if e.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
                return Err(e);
            }
        }
        Ok(Conn(&self.pipe))
    }
}

/// A connected client; read and write through `&File`.
pub struct Conn<'a>(&'a File);

impl Conn<'_> {
    pub fn file(&self) -> &File {
        self.0
    }
}

impl Drop for Conn<'_> {
    fn drop(&mut self) {
        let h = self.0.as_raw_handle();
        // SAFETY: our pipe handle. Flush first: disconnecting drops unread data on the client's side.
        unsafe {
            FlushFileBuffers(h);
            DisconnectNamedPipe(h);
        }
    }
}

/// Connects to the pipe `name`, waiting up to `wait` while all its instances are busy. `NotFound`: nobody serves it.
pub fn connect(name: &str, wait: Duration) -> io::Result<File> {
    let p = path(name);
    let open = || OpenOptions::new().read(true).write(true).open(&p);
    match open() {
        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
            // SAFETY: a NUL-terminated name.
            unsafe { WaitNamedPipeW(wide(&p).as_ptr(), wait.as_millis().min(u32::MAX as u128) as u32) };
            open()
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn round_trip_and_a_second_owner_is_refused() {
        let name = format!("dirigent-test-{}", std::process::id());
        let server = Server::bind(&name, true).unwrap();
        assert_eq!(Server::bind(&name, true).err().map(|e| e.kind()), Some(io::ErrorKind::AddrInUse));
        let t = std::thread::spawn(move || {
            for _ in 0..2 {
                let c = server.accept().unwrap();
                let mut line = String::new();
                BufReader::new(c.file()).read_line(&mut line).unwrap();
                c.file().write_all(format!("echo {line}").as_bytes()).unwrap();
            }
        });
        for i in 0..2 {
            let mut f = connect(&name, Duration::from_secs(5)).unwrap();
            writeln!(f, "hi {i}").unwrap();
            let mut reply = String::new();
            BufReader::new(f).read_line(&mut reply).unwrap();
            assert_eq!(reply, format!("echo hi {i}\n"));
        }
        t.join().unwrap();
        assert_eq!(
            connect("dirigent-test-nobody", Duration::from_millis(10)).err().map(|e| e.kind()),
            Some(io::ErrorKind::NotFound)
        );
    }
}
