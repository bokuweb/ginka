//! The main-thread bridge to the Sparkle framework embedded in `e1.app`.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{MainThreadMarker, msg_send};
use std::ffi::{CStr, CString, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

/// A running Sparkle standard updater controller.
///
/// Create and use this value on the AppKit main thread. Initialization returns
/// `None` for ordinary debug builds, bare executables and bundles without the
/// pinned Sparkle framework, so development cannot replace itself by accident.
pub struct Updater {
    controller: Retained<AnyObject>,
    _framework: Framework,
}

impl Updater {
    /// Load the bundled framework and start Sparkle's standard updater.
    pub fn init() -> Option<Self> {
        let forced = std::env::var_os("E1_FORCE_UPDATER").is_some_and(|value| value == "1");
        if cfg!(debug_assertions) && !forced {
            return None;
        }

        let _main_thread = MainThreadMarker::new()?;
        let framework = Framework::load(&sparkle_library_path()?).ok()?;
        let controller_class = AnyClass::get(c"SPUStandardUpdaterController")?;

        // SAFETY: Sparkle documents this initializer for programmatic setup.
        // Loading the framework above registers the class, AppKit is on the
        // main thread, and both optional delegates are deliberately nil.
        let controller = unsafe {
            let allocated: *mut AnyObject = msg_send![controller_class, alloc];
            let initialized: *mut AnyObject = msg_send![
                allocated,
                initWithStartingUpdater: true,
                updaterDelegate: std::ptr::null_mut::<AnyObject>(),
                userDriverDelegate: std::ptr::null_mut::<AnyObject>()
            ];
            Retained::from_raw(initialized)?
        };

        let this = Self {
            controller,
            _framework: framework,
        };
        this.check_at_launch_when_enabled();
        Some(this)
    }

    /// Open Sparkle's standard user-initiated update window.
    pub fn check_for_updates(&self) {
        // SAFETY: `controller` is a retained standard controller created on
        // the main thread, and the Objective-C action accepts a nil sender.
        let _: () = unsafe {
            msg_send![
                &*self.controller,
                checkForUpdates: std::ptr::null_mut::<AnyObject>()
            ]
        };
    }

    fn check_at_launch_when_enabled(&self) {
        // SAFETY: `updater` is a non-owning property retained by the live
        // controller. These selectors are part of Sparkle's documented core
        // API and this method runs immediately after main-thread creation.
        unsafe {
            let updater: *mut AnyObject = msg_send![&*self.controller, updater];
            let Some(updater) = updater.as_ref() else {
                return;
            };
            let enabled: bool = msg_send![updater, automaticallyChecksForUpdates];
            if enabled {
                let _: () = msg_send![updater, checkForUpdatesInBackground];
            }
        }
    }
}

/// The open framework handle must outlive every Objective-C object whose
/// class and implementation it registered.
struct Framework {
    _handle: *mut c_void,
}

impl Framework {
    fn load(path: &Path) -> Result<Self, String> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "Sparkle framework path contains a null byte".to_owned())?;
        // SAFETY: `path` is a live, null-terminated C string. The returned
        // handle is retained in process-lifetime state below.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: `dlerror` returns either null or a process-owned,
            // null-terminated diagnostic string valid until the next call.
            let message = unsafe {
                let error = libc::dlerror();
                if error.is_null() {
                    "unknown dynamic loader error".to_owned()
                } else {
                    CStr::from_ptr(error).to_string_lossy().into_owned()
                }
            };
            Err(message)
        } else {
            Ok(Self { _handle: handle })
        }
    }
}

// Do not call `dlclose`: Sparkle owns scheduled Objective-C objects that may
// outlive the controller wrapper. The framework is process-lifetime state and
// unloading its implementations during AppKit teardown would be unsafe.

/// Locate Sparkle beside a bundled executable:
/// `Contents/MacOS/e1` -> `Contents/Frameworks/Sparkle.framework/Sparkle`.
fn sparkle_library_path() -> Option<PathBuf> {
    sparkle_library_path_from(&std::env::current_exe().ok()?)
}

fn sparkle_library_path_from(executable: &Path) -> Option<PathBuf> {
    let library = sparkle_library_candidate(executable)?;
    library.exists().then_some(library)
}

fn sparkle_library_candidate(executable: &Path) -> Option<PathBuf> {
    let contents = executable.parent()?.parent()?;
    Some(contents.join("Frameworks/Sparkle.framework/Sparkle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_framework_is_beside_the_bundled_executable() {
        let executable = Path::new("/Applications/e1.app/Contents/MacOS/e1");
        assert_eq!(
            sparkle_library_candidate(executable),
            Some(PathBuf::from(
                "/Applications/e1.app/Contents/Frameworks/Sparkle.framework/Sparkle"
            ))
        );
    }
}
