#![cfg(test)]
// Copyright (c) 2017 CtrlC developers
// Licensed under the Apache License, Version 2.0
// <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT
// license <LICENSE-MIT or http://opensource.org/licenses/MIT>,
// at your option. All files in the project carrying such
// notice may not be copied, modified, or distributed except
// according to those terms.

#[cfg(unix)]
mod platform {
    use std::io;

    pub fn setup() -> io::Result<()> {
        Ok(())
    }

    pub fn cleanup() -> io::Result<()> {
        Ok(())
    }

    pub fn raise_ctrl_c() {
        nix::sys::signal::raise(nix::sys::signal::SIGINT).unwrap();
    }

    pub fn print(fmt: ::std::fmt::Arguments) {
        use self::io::Write;
        let stdout = ::std::io::stdout();
        stdout.lock().write_fmt(fmt).unwrap();
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::ptr;

    use winapi::shared::minwindef::DWORD;
    use winapi::shared::ntdef::{CHAR, HANDLE};
    use winapi::um::consoleapi::{AllocConsole, GetConsoleMode};
    use winapi::um::fileapi::WriteFile;
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::processenv::{GetStdHandle, SetStdHandle};
    use winapi::um::winbase::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::wincon::{AttachConsole, FreeConsole, GenerateConsoleCtrlEvent};

    /// Stores a piped stdout handle or a cache that gets
    /// flushed when we reattached to the old console.
    enum Output {
        Pipe(HANDLE),
        Cached(Vec<u8>),
    }

    static mut OLD_OUT: *mut Output = 0 as *mut Output;

    impl io::Write for Output {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match *self {
                Output::Pipe(handle) => unsafe {
                    use winapi::shared::ntdef::VOID;

                    let mut n = 0u32;
                    if WriteFile(
                        handle,
                        buf.as_ptr() as *const VOID,
                        buf.len() as DWORD,
                        &mut n as *mut DWORD,
                        ptr::null_mut(),
                    ) == 0
                    {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                },
                Output::Cached(ref mut s) => s.write(buf),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Output {
        /// Stores current piped stdout or creates a new output cache that will
        /// be written to stdout at a later time.
        fn new() -> io::Result<Output> {
            unsafe {
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                if stdout.is_null() || stdout == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }

                let mut out = 0u32;
                match GetConsoleMode(stdout, &mut out as *mut DWORD) {
                    0 => Ok(Output::Pipe(stdout)),
                    _ => Ok(Output::Cached(Vec::new())),
                }
            }
        }

        /// Set stdout/stderr and flush cache.
        unsafe fn set_as_std(self) -> io::Result<()> {
            let stdout = match self {
                Output::Pipe(h) => h,
                Output::Cached(_) => get_stdout()?,
            };

            if SetStdHandle(STD_OUTPUT_HANDLE, stdout) == 0 {
                return Err(io::Error::last_os_error());
            }

            if SetStdHandle(STD_ERROR_HANDLE, stdout) == 0 {
                return Err(io::Error::last_os_error());
            }

            match self {
                Output::Pipe(_) => Ok(()),
                Output::Cached(ref s) => {
                    // Write cached output
                    use self::io::Write;
                    let out = io::stdout();
                    out.lock().write_all(&s[..])?;
                    Ok(())
                }
            }
        }
    }

    unsafe fn get_stdout() -> io::Result<HANDLE> {
        use winapi::um::fileapi::{CreateFileA, OPEN_EXISTING};
        use winapi::um::handleapi::INVALID_HANDLE_VALUE;
        use winapi::um::winnt::{FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE};

        let stdout = CreateFileA(
            "CONOUT$\0".as_ptr() as *const CHAR,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_WRITE,
            ptr::null_mut(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        );

        if stdout.is_null() || stdout == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(stdout)
        }
    }

    /// Detach from the current console and create a new one,
    /// We do this because GenerateConsoleCtrlEvent() sends ctrl-c events
    /// to all processes on the same console. We want events to be received
    /// only by our process.
    ///
    /// This breaks rust's stdout pre 1.18.0. Rust used to
    /// [cache the std handles](https://github.com/rust-lang/rust/pull/40516)
    ///
    pub fn setup() -> io::Result<()> {
        unsafe {
            let old_out = Output::new()?;

            if FreeConsole() == 0 {
                return Err(io::Error::last_os_error());
            }

            if AllocConsole() == 0 {
                return Err(io::Error::last_os_error());
            }

            // AllocConsole will not always set stdout/stderr to the to the console buffer
            // of the new terminal.

            let stdout = get_stdout()?;
            if SetStdHandle(STD_OUTPUT_HANDLE, stdout) == 0 {
                return Err(io::Error::last_os_error());
            }

            if SetStdHandle(STD_ERROR_HANDLE, stdout) == 0 {
                return Err(io::Error::last_os_error());
            }

            OLD_OUT = Box::into_raw(Box::new(old_out));

            Ok(())
        }
    }

    /// Reattach to the old console.
    pub fn cleanup() -> io::Result<()> {
        unsafe {
            if FreeConsole() == 0 {
                return Err(io::Error::last_os_error());
            }

            if AttachConsole(winapi::um::wincon::ATTACH_PARENT_PROCESS) == 0 {
                return Err(io::Error::last_os_error());
            }

            Box::from_raw(OLD_OUT).set_as_std()?;

            Ok(())
        }
    }

    /// This will signal the whole process group.
    pub fn raise_ctrl_c() {
        unsafe {
            assert!(GenerateConsoleCtrlEvent(winapi::um::wincon::CTRL_C_EVENT, 0) != 0);
        }
    }

    /// Print to both consoles, this is not thread safe.
    pub fn print(fmt: ::std::fmt::Arguments) {
        unsafe {
            use self::io::Write;
            {
                let stdout = io::stdout();
                stdout.lock().write_fmt(fmt).unwrap();
            }
            {
                assert!(!OLD_OUT.is_null());
                (*OLD_OUT).write_fmt(fmt).unwrap();
            }
        }
    }
}

#[cfg(not(unix))]
#[cfg(not(windows))]
mod platform {
    use std::io;

    pub fn setup() -> io::Result<()> {
        Ok(())
    }

    pub fn cleanup() -> io::Result<()> {
        Ok(())
    }

    pub fn raise_ctrl_c() {
    }

    pub fn print(fmt: ::std::fmt::Arguments) {
        use self::io::Write;
        let stdout = ::std::io::stdout();
        stdout.lock().write_fmt(fmt).unwrap();
    }
}

fn test_set_handler()
{
    let (tx, rx) = ::std::sync::mpsc::channel();
    crate::set_handler(move || {
        tx.send(true).unwrap();
    })
    .unwrap();

    let nothing = rx.recv_timeout(::std::time::Duration::from_millis(100));
    assert!(nothing.is_err(), "should not have been triggered yet");

    platform::raise_ctrl_c();

    rx.recv_timeout(::std::time::Duration::from_secs(10))
        .unwrap();

    match crate::set_handler(|| {}) {
        Err(crate::Error::MultipleHandlers) => {}
        Err(err) => panic!("{:?}", err),
        Ok(_) => panic!("should not have succeeded"),
    }
}

fn test_set_async_handler_wrapper()
{
    #[cfg(feature = "tokio")]
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(test_set_async_handler());
    }

    #[cfg(feature = "async-std")]
    async_std::task::block_on(test_set_async_handler());
}

async fn test_set_async_handler()
{
    #[cfg(feature = "tokio")]
    let (tx, mut rx) = ::tokio::sync::mpsc::channel(1);
    #[cfg(feature = "async-std")]
    let (tx, rx) = async_std::channel::bounded(1);

    crate::set_async_handler(async move {
        tx.send(true).await.unwrap();       
    })
    .unwrap();

    let nothing = crate::helper::timeout(::std::time::Duration::from_millis(100), rx.recv())
        .await;
    assert!(nothing.is_none(), "should not have been triggered yet");

    platform::raise_ctrl_c();

    crate::helper::timeout(::std::time::Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();

    match crate::set_async_handler(async {}) {
        Err(crate::Error::MultipleHandlers) => {}
        Err(err) => panic!("{:?}", err),
        Ok(_) => panic!("should not have succeeded"),
    }
}

macro_rules! run_tests {
    ( $($test_fn:ident),* ) => {
        platform::print(format_args!("\n"));
        $(
            platform::print(format_args!("test tests::{} ... ", stringify!($test_fn)));
            $test_fn();
            platform::print(format_args!("ok\n"));
        )*
        platform::print(format_args!("\n"));
    }
}

use rusty_fork::rusty_fork_test;
rusty_fork_test! {
    #[test]
    fn test_sync() {
        platform::setup().unwrap();

        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            platform::cleanup().unwrap();
            (default)(info);
        }));

        run_tests!(test_set_handler);

        platform::cleanup().unwrap();
    }

    #[test]
    fn test_async() {
        platform::setup().unwrap();

        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            platform::cleanup().unwrap();
            (default)(info);
        }));

        run_tests!(test_set_async_handler_wrapper);

        platform::cleanup().unwrap();
    }
}