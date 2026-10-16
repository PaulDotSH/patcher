//! OS integration: write checks, opening folders and links, launching the game, the
//! clipboard, and (on Windows) relaunching as administrator.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Whether files can be created in `dir`. On Linux this is always assumed (no elevation is
/// offered there; a permission error shows on the error screen instead).
pub fn writable(dir: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let probe = dir.join(".patcher-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(e) => e.kind() != std::io::ErrorKind::PermissionDenied,
    }
}

/// Opens a folder or a URL with the system's default handler.
pub fn open(target: &str) {
    let r = if cfg!(windows) {
        Command::new("explorer").arg(target).spawn()
    } else {
        Command::new("xdg-open").arg(target).spawn()
    };
    if let Err(e) = r {
        eprintln!("cannot open {target}: {e}");
    }
}

/// Starts `program` (relative to `game`) with the game folder as its working directory.
pub fn launch(game: &Path, program: &str) -> std::io::Result<()> {
    let exe = patcher_core::paths::join(game, &program.replace('\\', "/"));
    Command::new(exe).current_dir(game).spawn().map(drop)
}

pub fn copy(text: &str) {
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        eprintln!("clipboard: {e}");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ElevateError {
    Declined,
    Unsupported,
    Failed(String),
}

/// Quotes one argument for a Windows command line (the rules `CommandLineToArgvW` reads).
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Starts this executable again as administrator with `args`. Returns once the UAC prompt
/// was accepted and the new process started.
#[cfg(windows)]
pub fn relaunch_elevated(args: &[OsString]) -> Result<(), ElevateError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let exe = std::env::current_exe().map_err(|e| ElevateError::Failed(e.to_string()))?;
    let params: Vec<String> = args.iter().map(|a| quote_arg(&a.to_string_lossy())).collect();
    let (file, params, verb) = (wide(exe.as_os_str()), wide(params.join(" ").as_ref()), wide("runas".as_ref()));
    // SAFETY: every pointer refers to a live, NUL-terminated buffer above; the struct is
    // zero-initialised as the API allows.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        if ShellExecuteExW(&mut info) != 0 {
            Ok(())
        } else if GetLastError() == ERROR_CANCELLED {
            Err(ElevateError::Declined)
        } else {
            Err(ElevateError::Failed(std::io::Error::last_os_error().to_string()))
        }
    }
}

#[cfg(not(windows))]
pub fn relaunch_elevated(_args: &[OsString]) -> Result<(), ElevateError> {
    Err(ElevateError::Unsupported)
}

/// Shows `text` in a native message box (stderr off Windows). Used when the window itself
/// cannot be built.
#[cfg(windows)]
pub fn message_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (t, m) = (wide(title), wide(text));
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
pub fn message_box(title: &str, text: &str) {
    eprintln!("{title}: {text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_quoting() {
        assert_eq!(quote_arg("--game"), "--game");
        assert_eq!(quote_arg(r"C:\Program Files\Game"), r#""C:\Program Files\Game""#);
        assert_eq!(quote_arg(r"C:\Program Files\Game\"), r#""C:\Program Files\Game\\""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(""), r#""""#);
    }
}
