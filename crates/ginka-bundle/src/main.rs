//! Assemble `Ginka.app` around the Chromium framework and the helper apps the
//! workspace browser needs (`scripts/package-macos`, `scripts/dev-macos`).
//!
//! Chromium finds its framework under `Contents/Frameworks` and starts its
//! processes as `<executable> Helper*.app` beside it, so the bundle is laid
//! out the way it expects; the command line and the daemon are added by the
//! scripts afterwards.
//!
//! ```text
//! ginka-bundle --binaries DIR --out DIR --executable ginka-app \
//!     --helper ginka-helper --identifier ID --version 1.2.3
//! ```

#[cfg(target_os = "macos")]
fn main() {
    use cef::build_util::mac::{BundleInfo, bundle};
    use std::path::PathBuf;

    let mut binaries = None;
    let mut out = None;
    let mut executable = "ginka-app".to_string();
    let mut helper = "ginka-helper".to_string();
    let mut identifier = "io.github.bokuweb.ginka".to_string();
    let mut version = "0.0.0".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else {
            fail(&format!("{flag} needs a value"));
        };
        match flag.as_str() {
            "--binaries" => binaries = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--executable" => executable = value,
            "--helper" => helper = value,
            "--identifier" => identifier = value,
            "--version" => version = value,
            _ => fail(&format!("unknown flag {flag}")),
        }
    }
    let (Some(binaries), Some(out)) = (binaries, out) else {
        fail("--binaries and --out are required");
    };
    let version = semver::Version::parse(&version)
        .unwrap_or_else(|error| fail(&format!("version {version}: {error}")));
    if let Err(error) = std::fs::create_dir_all(&out) {
        fail(&format!("{}: {error}", out.display()));
    }
    let info = BundleInfo::new(&executable, &identifier, "Ginka", "en", version);
    match bundle(&out, &binaries, &executable, &helper, None, info) {
        Ok(app) => println!("{}", app.display()),
        Err(error) => fail(&format!("assembling the app: {error:?}")),
    }
}

#[cfg(target_os = "macos")]
fn fail(message: &str) -> ! {
    eprintln!("ginka-bundle: {message}");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ginka-bundle only assembles the macOS app");
}
