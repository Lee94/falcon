use std::io::{self, Write};

pub(super) struct Output {
    stdout: io::Stdout,
    #[cfg(windows)]
    utf8: super::input::Utf8Input,
}

impl Output {
    pub(super) fn new() -> Self {
        Self {
            stdout: io::stdout(),
            #[cfg(windows)]
            utf8: super::input::Utf8Input::default(),
        }
    }

    pub(super) fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_all(bytes)?;
        self.flush()
    }

    pub(super) fn replay(&mut self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(windows)]
        self.utf8.clear();
        self.write_all(super::RESET)?;
        super::replay::write(self, bytes)?;
        self.flush()
    }
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            // WriteConsoleW requires complete UTF-8, unlike a Unix byte stream.
            let text = self.utf8.push(bytes.to_vec());
            self.stdout.lock().write_all(text.as_bytes())?;
            Ok(bytes.len())
        }
        #[cfg(not(windows))]
        self.stdout.lock().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.lock().flush()
    }
}
