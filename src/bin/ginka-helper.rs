//! The program Chromium starts for each of its own processes — renderer, GPU,
//! network — when the workspace browser runs (`docs/ui.md` §3.4).
//!
//! macOS wants those in a separate executable, shipped inside the app as
//! `Contents/Frameworks/ginka-app Helper*.app`; `scripts/package-macos` puts
//! this binary there. Elsewhere there is nothing for it to do.

#[cfg(target_os = "macos")]
fn main() {
    use cef::{App, api_hash, args::Args, execute_process, library_loader, sandbox, sys};

    let args = Args::new();
    // Chromium's children run sandboxed; a helper that skips this has its GPU
    // and network processes exit at once, and no page ever paints.
    let mut sandbox = sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());

    // A helper sits three directories below the framework it loads.
    let Ok(exe) = std::env::current_exe() else {
        std::process::exit(1);
    };
    let loader = library_loader::LibraryLoader::new(&exe, true);
    if !loader.load() {
        std::process::exit(1);
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let code = execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );
    drop(loader);
    drop(sandbox);
    std::process::exit(code);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ginka-helper only runs inside the macOS app, for its browser");
}
