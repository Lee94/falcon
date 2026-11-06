use std::io;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
};

// Raw byte forwarding requires Windows' VT input and output modes, in addition
// to crossterm's raw mode. Preserve the original flags for the enclosing Tern shell.
pub(super) struct ConsoleModes {
    input: HANDLE,
    output: HANDLE,
    input_mode: u32,
    output_mode: u32,
}

impl ConsoleModes {
    pub(super) fn capture() -> io::Result<Self> {
        // SAFETY: these are the process console handles, used only with console APIs.
        unsafe {
            let input = GetStdHandle(STD_INPUT_HANDLE);
            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            if input.is_null() || output.is_null() || input == INVALID_HANDLE_VALUE || output == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut input_mode = 0;
            let mut output_mode = 0;
            if GetConsoleMode(input, &mut input_mode) == 0 || GetConsoleMode(output, &mut output_mode) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { input, output, input_mode, output_mode })
        }
    }

    pub(super) fn enable_virtual_terminal(&self) -> io::Result<()> {
        // crossterm has already disabled line/echo/processed input; retain those flags.
        // SAFETY: handles and storage were validated by capture().
        unsafe {
            let mut raw_mode = 0;
            if GetConsoleMode(self.input, &mut raw_mode) == 0
                || SetConsoleMode(self.input, raw_mode | ENABLE_VIRTUAL_TERMINAL_INPUT) == 0
                || SetConsoleMode(self.output, self.output_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

impl Drop for ConsoleModes {
    fn drop(&mut self) {
        // SAFETY: these borrowed standard handles remain owned by the process.
        unsafe {
            SetConsoleMode(self.input, self.input_mode);
            SetConsoleMode(self.output, self.output_mode);
        }
    }
}
