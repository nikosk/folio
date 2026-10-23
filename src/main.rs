mod app;
mod doc;
mod headless;
mod highlight;
mod layout;
mod paint;
mod tex;
mod theme;
mod view;

use std::path::Path;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

pub static START: OnceLock<Instant> = OnceLock::new();

/// Startup profiling: prints elapsed time at named points when FOLIO_STARTUP is set.
pub fn mark(label: &str) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("FOLIO_STARTUP").is_some()) {
        let ms = START.get().map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        eprintln!("{ms:7.1} ms  {label}");
    }
}

fn main() -> ExitCode {
    START.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--render" || a == "--bench") {
        return headless::run(&args);
    }
    // Start reading and parsing the file while GTK initialises.
    if let Some(f) = args.iter().find(|a| !a.starts_with('-')) {
        app::preload(Path::new(f));
    }
    match app::run() {
        gtk::glib::ExitCode::SUCCESS => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
