#![windows_subsystem = "windows"]

#[cfg(windows)]
fn main() -> eframe::Result {
    lords_command::run_standalone()
}

/// The window is a Windows app (D27): elsewhere this says so, exit 1, and points to the panel's server, which any OS
/// runs.
#[cfg(not(windows))]
fn main() -> Result<(), String> {
    Err("lords-command: the window runs on Windows; here, run `lords panel serve` and open the window on a Windows PC"
        .into())
}
