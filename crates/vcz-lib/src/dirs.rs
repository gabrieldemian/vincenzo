//! Stolen from https://github.com/sassman/dirs-lite-rs

use std::{env, path::PathBuf};

const CONFIG_DIR: &str = ".config";
const DATA_DIR: &str = ".local/share";
const DOWNLOAD_DIR: &str = "Downloads";
// const CACHE_DIR: &str = ".cache";

/// Returns the path to the user's config directory.
///
/// The returned value depends on the operating system and is either a `Some`,
/// containing a value from the following table, or a `None`.
///
/// |Platform | Value                                 | Example                                  |
/// | ------- | ------------------------------------- | ---------------------------------------- |
/// | Linux   | `$XDG_CONFIG_HOME` or `$HOME`/.config | /home/alice/.config                      |
/// | macOS   | `$HOME`/Library/Application Support   | /Users/Alice/Library/Application Support |
/// | Windows | `%APPDATA%`\Roaming                   | C:\Users\Alice\AppData\Roaming           |
///
/// NOTE: if the feature `favor-xdg-style` is enabled, `$HOME/.config` is
/// favorized.
pub(crate) fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        // Linux: Use $HOME/.config
        let from_xdg = env::var_os("XDG_CONFIG_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        if let Some(config_path) = from_xdg {
            return Some(config_path);
        }
        std::env::home_dir().map(|mut v| {
            v.push(CONFIG_DIR);
            v
        })
    } else if cfg!(target_os = "macos") {
        // macOS: Use $HOME/Library/Application Support
        //  or $HOME/.config if favor-xdg-style is enabled
        std::env::home_dir().map(|mut home| {
            if cfg!(feature = "favor-xdg-style") {
                home.push(CONFIG_DIR);
                return home;
            }
            home.push("Library");
            home.push("Application Support");
            home
        })
    } else if cfg!(target_os = "windows") {
        // Windows: Use %APPDATA%
        env::var_os("APPDATA").filter(|s| !s.is_empty()).map(PathBuf::from)
    } else {
        // Unsupported platform
        None
    }
}

/// Returns the path to the user's data directory.
///
/// The returned value depends on the operating system and is either a `Some`,
/// containing a value from the following table, or a `None`.
///
/// |Platform | Value                                 | Example                                  |
/// | ------- | ------------------------------------- | ---------------------------------------- |
/// | Linux   | `$XDG_DATA_HOME` or `$HOME`/.local/share | /home/alice/.local/share              |
/// | macOS   | `$HOME`/Library/Application Support   | /Users/Alice/Library/Application Support |
/// | Windows | `%LOCALAPPDATA%`                      | C:\Users\Alice\AppData\Local             |
///
/// NOTE: if the feature `favor-xdg-style` is enabled, `$HOME/.local/share` is
/// favorized on macOS.
pub(crate) fn data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        // Linux: Use $XDG_DATA_HOME or $HOME/.local/share
        env::var_os("XDG_DATA_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::home_dir().map(|mut home| {
                    home.push(DATA_DIR);
                    home
                })
            })
    } else if cfg!(target_os = "macos") {
        // macOS: Use $HOME/Library/Application Support
        //  or $HOME/.local/share if favor-xdg-style is enabled
        std::env::home_dir().map(|mut home| {
            if cfg!(feature = "favor-xdg-style") {
                home.push(DATA_DIR);
                return home;
            }
            home.push("Library");
            home.push("Application Support");
            home
        })
    } else if cfg!(target_os = "windows") {
        // Windows: Use %LOCALAPPDATA%
        env::var_os("LOCALAPPDATA").filter(|s| !s.is_empty()).map(PathBuf::from)
    } else {
        // Unsupported platform
        None
    }
}

/// |Platform | Value                  | Example                  |
/// | ------- | ---------------------- | ------------------------ |
/// | Linux   | `XDG_DOWNLOAD_DIR`     | /home/alice/Downloads    |
/// | macOS   | `$HOME`/Downloads      | /Users/Alice/Downloads   |
/// | Windows | `{FOLDERID_Downloads}` | C:\Users\Alice\Downloads |
pub(crate) fn download_dir() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        // Linux: Use $XDG_DOWNLOAD_DIR or $HOME/Downloads
        env::var_os("XDG_DOWNLOAD_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::home_dir().map(|mut home| {
                    home.push(DOWNLOAD_DIR);
                    home
                })
            })
    } else if cfg!(target_os = "macos") {
        // The same for MacOS
        std::env::home_dir().map(|mut home| {
            home.push(DOWNLOAD_DIR);
            home
        })
    } else if cfg!(target_os = "windows") {
        std::env::home_dir().map(|mut home| {
            home.push(DOWNLOAD_DIR);
            home
        })
    } else {
        // Unsupported platform
        None
    }
}

// Returns the path to the user's cache directory.
//
// The returned value depends on the operating system and is either a `Some`,
// containing a value from the following table, or a `None`.
//
// |Platform | Value                                 | Example                                  |
// | ------- | ------------------------------------- | ---------------------------------------- |
// | Linux   | `$XDG_CACHE_HOME` or `$HOME`/.cache   | /home/alice/.cache                       |
// | macOS   | `$HOME`/Library/Caches                | /Users/Alice/Library/Caches              |
// | Windows | `%LOCALAPPDATA%`                      | C:\Users\Alice\AppData\Local             |
//
// NOTE: if the feature `favor-xdg-style` is enabled, `$HOME/.cache` is
// favorized on macOS.
// pub(crate) fn cache_dir() -> Option<PathBuf> {
//     if cfg!(target_os = "linux") {
//         // Linux: Use $XDG_CACHE_HOME or $HOME/.cache
//         env::var_os("XDG_CACHE_HOME")
//             .filter(|s| !s.is_empty())
//             .map(PathBuf::from)
//             .or_else(|| {
//                 std::env::home_dir().map(|mut home| {
//                     home.push(CACHE_DIR);
//                     home
//                 })
//             })
//     } else if cfg!(target_os = "macos") {
//         // macOS: Use $HOME/Library/Caches
//         //  or $HOME/.cache if favor-xdg-style is enabled
//         std::env::home_dir().map(|mut home| {
//             if cfg!(feature = "favor-xdg-style") {
//                 home.push(CACHE_DIR);
//                 return home;
//             }
//             home.push("Library");
//             home.push("Caches");
//             home
//         })
//     } else if cfg!(target_os = "windows") {
//         // Windows: Use %LOCALAPPDATA%
//         env::var_os("LOCALAPPDATA").filter(|s| !s.is_empty()).map(PathBuf::from)
//     } else {
//         // Unsupported platform
//         None
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn set_var(key: &str, value: &str) {
        unsafe { env::set_var(key, value) };
    }

    unsafe fn remove_var(key: &str) {
        unsafe { env::remove_var(key) };
    }

    fn restore_var(key: &str, original: Option<String>) {
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            match original {
                Some(val) => set_var(key, &val),
                None => remove_var(key),
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn restore_var_os(key: &str, original: Option<std::ffi::OsString>) {
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            match original {
                Some(val) => env::set_var(key, val),
                None => env::remove_var(key),
            }
        }
    }

    #[test]
    #[ignore]
    fn config_dir_returns_some() {
        let result = config_dir();
        assert!(
            result.is_some(),
            "config_dir should return Some on supported platforms"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_uses_xdg_config_home_when_set() {
        let original = env::var("XDG_CONFIG_HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("XDG_CONFIG_HOME", "/custom/config") };

        let result = config_dir();
        assert_eq!(result, Some(PathBuf::from("/custom/config")));

        restore_var("XDG_CONFIG_HOME", original);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_falls_back_to_home_when_xdg_unset() {
        let original_xdg = env::var("XDG_CONFIG_HOME").ok();
        let original_home = env::var("HOME").ok();

        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            remove_var("XDG_CONFIG_HOME");
            set_var("HOME", "/home/testuser");
        }

        let result = config_dir();
        assert_eq!(result, Some(PathBuf::from("/home/testuser/.config")));

        restore_var("XDG_CONFIG_HOME", original_xdg);
        restore_var("HOME", original_home);
    }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "favor-xdg-style")))]
    #[ignore]
    fn macos_config_dir_uses_library_application_support() {
        let original = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("HOME", "/Users/testuser") };

        let result = config_dir();
        assert_eq!(
            result,
            Some(PathBuf::from("/Users/testuser/Library/Application Support"))
        );

        restore_var("HOME", original);
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "favor-xdg-style"))]
    #[ignore]
    fn macos_config_dir_uses_xdg_style() {
        let original = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("HOME", "/Users/testuser") };

        let result = config_dir();
        assert_eq!(result, Some(PathBuf::from("/Users/testuser/.config")));

        restore_var("HOME", original);
    }

    #[test]
    #[cfg(target_os = "windows")]
    #[ignore]
    fn windows_uses_appdata() {
        let original = env::var("APPDATA").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("APPDATA", "C:\\Users\\testuser\\AppData\\Roaming") };

        let result = config_dir();
        assert_eq!(
            result,
            Some(PathBuf::from("C:\\Users\\testuser\\AppData\\Roaming"))
        );

        restore_var("APPDATA", original);
    }

    #[test]
    #[ignore]
    fn config_dir_path_is_absolute() {
        let result = config_dir();
        if let Some(path) = result {
            assert!(
                path.is_absolute(),
                "config_dir should return an absolute path"
            );
        }
    }

    #[test]
    #[ignore]
    fn data_dir_returns_some() {
        let result = data_dir();
        assert!(
            result.is_some(),
            "data_dir should return Some on supported platforms"
        );
    }

    #[test]
    #[ignore]
    fn download_dir_returns_some() {
        let result = download_dir();
        assert!(
            result.is_some(),
            "download_dir should return Some on supported platforms"
        );
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn linux_data_dir_uses_xdg_data_home_when_set() {
        let original = env::var("XDG_DATA_HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("XDG_DATA_HOME", "/custom/data") };

        let result = data_dir();
        assert_eq!(result, Some(PathBuf::from("/custom/data")));

        restore_var("XDG_DATA_HOME", original);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_download_dir_uses_xdg_download_dir_when_set() {
        let original = env::var("XDG_DOWNLOAD_DIR").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("XDG_DOWNLOAD_DIR", "/custom/data") };

        let result = download_dir();
        assert_eq!(result, Some(PathBuf::from("/custom/data")));

        restore_var("XDG_DOWNLOAD_DIR", original);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_download_dir_falls_back_to_home_when_xdg_unset() {
        let original_xdg = env::var("XDG_DOWNLOAD_DIR").ok();
        let original_home = env::var("HOME").ok();

        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            remove_var("XDG_DOWNLOAD_DIR");
            set_var("HOME", "/home/testuser");
        }

        let result = download_dir();
        assert_eq!(result, Some(PathBuf::from("/home/testuser/Downloads")));

        restore_var("XDG_DOWNLOAD_DIR", original_xdg);
        restore_var("HOME", original_home);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_data_dir_falls_back_to_home_when_xdg_unset() {
        let original_xdg = env::var("XDG_DATA_HOME").ok();
        let original_home = env::var("HOME").ok();

        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            remove_var("XDG_DATA_HOME");
            set_var("HOME", "/home/testuser");
        }

        let result = data_dir();
        assert_eq!(result, Some(PathBuf::from("/home/testuser/.local/share")));

        restore_var("XDG_DATA_HOME", original_xdg);
        restore_var("HOME", original_home);
    }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "favor-xdg-style")))]
    #[ignore]
    fn macos_data_dir_uses_library_application_support() {
        let original = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("HOME", "/Users/testuser") };

        let result = data_dir();
        assert_eq!(
            result,
            Some(PathBuf::from("/Users/testuser/Library/Application Support"))
        );

        restore_var("HOME", original);
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "favor-xdg-style"))]
    #[ignore]
    fn macos_data_dir_uses_xdg_style() {
        let original = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("HOME", "/Users/testuser") };

        let result = data_dir();
        assert_eq!(result, Some(PathBuf::from("/Users/testuser/.local/share")));

        restore_var("HOME", original);
    }

    #[test]
    #[cfg(target_os = "windows")]
    #[ignore]
    fn windows_data_dir_uses_localappdata() {
        let original = env::var("LOCALAPPDATA").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            set_var("LOCALAPPDATA", "C:\\Users\\runneradmin\\AppData\\Local")
        };

        let result = data_dir();
        assert_eq!(
            result,
            Some(PathBuf::from("C:\\Users\\runneradmin\\AppData\\Local"))
        );

        restore_var("LOCALAPPDATA", original);
    }

    #[test]
    #[ignore]
    fn data_dir_path_is_absolute() {
        let result = data_dir();
        if let Some(path) = result {
            assert!(
                path.is_absolute(),
                "data_dir should return an absolute path"
            );
        }
    }

    // #[test]
    // fn cache_dir_returns_some() {
    //     let result = cache_dir();
    //     assert!(
    //         result.is_some(),
    //         "cache_dir should return Some on supported platforms"
    //     );
    // }

    // #[test]
    // #[cfg(target_os = "linux")]
    // fn linux_cache_dir_uses_xdg_cache_home_when_set() {
    //     let original = env::var("XDG_CACHE_HOME").ok();
    //     // SAFETY: Tests run single-threaded with --test-threads=1
    //     unsafe { set_var("XDG_CACHE_HOME", "/custom/cache") };
    //
    //     let result = cache_dir();
    //     assert_eq!(result, Some(PathBuf::from("/custom/cache")));
    //
    //     restore_var("XDG_CACHE_HOME", original);
    // }

    // #[test]
    // #[cfg(target_os = "linux")]
    // fn linux_cache_dir_falls_back_to_home_when_xdg_unset() {
    //     let original_xdg = env::var("XDG_CACHE_HOME").ok();
    //     let original_home = env::var("HOME").ok();
    //
    //     // SAFETY: Tests run single-threaded with --test-threads=1
    //     unsafe {
    //         remove_var("XDG_CACHE_HOME");
    //         set_var("HOME", "/home/testuser");
    //     }
    //
    //     let result = cache_dir();
    //     assert_eq!(result, Some(PathBuf::from("/home/testuser/.cache")));
    //
    //     restore_var("XDG_CACHE_HOME", original_xdg);
    //     restore_var("HOME", original_home);
    // }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "favor-xdg-style")))]
    #[ignore]
    fn macos_cache_dir_uses_library_caches() {
        let original = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("HOME", "/Users/testuser") };

        let result = cache_dir();
        assert_eq!(
            result,
            Some(PathBuf::from("/Users/testuser/Library/Caches"))
        );

        restore_var("HOME", original);
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "favor-xdg-style"))]
    #[ignore]
    fn macos_cache_dir_uses_xdg_style() {
        let original = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { set_var("HOME", "/Users/testuser") };

        let result = cache_dir();
        assert_eq!(result, Some(PathBuf::from("/Users/testuser/.cache")));

        restore_var("HOME", original);
    }

    #[test]
    #[cfg(target_os = "windows")]
    #[ignore]
    fn windows_cache_dir_uses_localappdata() {
        let original = env::var("LOCALAPPDATA").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            set_var("LOCALAPPDATA", "C:\\Users\\testuser\\AppData\\Local")
        };

        let result = cache_dir();
        assert_eq!(
            result,
            Some(PathBuf::from("C:\\Users\\testuser\\AppData\\Local"))
        );

        restore_var("LOCALAPPDATA", original);
    }

    // #[test]
    // fn cache_dir_path_is_absolute() {
    //     let result = cache_dir();
    //     if let Some(path) = result {
    //         assert!(
    //             path.is_absolute(),
    //             "cache_dir should return an absolute path"
    //         );
    //     }
    // }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_config_dir_handles_non_utf8_xdg() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let original = env::var_os("XDG_CONFIG_HOME");
        let non_utf8 = OsStr::from_bytes(b"/tmp/\xff\xfe");
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { env::set_var("XDG_CONFIG_HOME", non_utf8) };

        let result = config_dir();
        let expected = PathBuf::from(non_utf8);
        assert_eq!(result, Some(expected));

        restore_var_os("XDG_CONFIG_HOME", original);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_data_dir_handles_non_utf8_xdg() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let original = env::var_os("XDG_DATA_HOME");
        let non_utf8 = OsStr::from_bytes(b"/tmp/\xff\xfe/data");
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { env::set_var("XDG_DATA_HOME", non_utf8) };

        let result = data_dir();
        assert_eq!(result, Some(PathBuf::from(non_utf8)));

        restore_var_os("XDG_DATA_HOME", original);
    }

    // #[test]
    // #[cfg(target_os = "linux")]
    // fn linux_cache_dir_handles_non_utf8_xdg() {
    //     use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    //
    //     let original = env::var_os("XDG_CACHE_HOME");
    //     let non_utf8 = OsStr::from_bytes(b"/tmp/\xff\xfe/cache");
    //     // SAFETY: Tests run single-threaded with --test-threads=1
    //     unsafe { env::set_var("XDG_CACHE_HOME", non_utf8) };
    //
    //     let result = cache_dir();
    //     assert_eq!(result, Some(PathBuf::from(non_utf8)));
    //
    //     restore_var_os("XDG_CACHE_HOME", original);
    // }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "favor-xdg-style")))]
    #[ignore]
    fn macos_config_dir_handles_non_utf8_home() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let original = env::var_os("HOME");
        let non_utf8_home = OsStr::from_bytes(b"/Users/\xff\xfe");
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { env::set_var("HOME", non_utf8_home) };

        let result = config_dir();
        let mut expected = PathBuf::from(non_utf8_home);
        expected.push("Library");
        expected.push("Application Support");
        assert_eq!(result, Some(expected));

        restore_var_os("HOME", original);
    }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "favor-xdg-style")))]
    #[ignore]
    fn macos_data_dir_handles_non_utf8_home() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let original = env::var_os("HOME");
        let non_utf8_home = OsStr::from_bytes(b"/Users/\xff\xfe");
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { env::set_var("HOME", non_utf8_home) };

        let result = data_dir();
        let mut expected = PathBuf::from(non_utf8_home);
        expected.push("Library");
        expected.push("Application Support");
        assert_eq!(result, Some(expected));

        restore_var_os("HOME", original);
    }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "favor-xdg-style")))]
    #[ignore]
    fn macos_cache_dir_handles_non_utf8_home() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let original = env::var_os("HOME");
        let non_utf8_home = OsStr::from_bytes(b"/Users/\xff\xfe");
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe { env::set_var("HOME", non_utf8_home) };

        let result = cache_dir();
        let mut expected = PathBuf::from(non_utf8_home);
        expected.push("Library");
        expected.push("Caches");
        assert_eq!(result, Some(expected));

        restore_var_os("HOME", original);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_config_dir_ignores_empty_xdg() {
        let original_xdg = env::var("XDG_CONFIG_HOME").ok();
        let original_home = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            set_var("XDG_CONFIG_HOME", "");
            set_var("HOME", "/home/testuser");
        }

        let result = config_dir();
        assert_eq!(result, Some(PathBuf::from("/home/testuser/.config")));

        restore_var("XDG_CONFIG_HOME", original_xdg);
        restore_var("HOME", original_home);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore]
    fn linux_data_dir_ignores_empty_xdg() {
        let original_xdg = env::var("XDG_DATA_HOME").ok();
        let original_home = env::var("HOME").ok();
        // SAFETY: Tests run single-threaded with --test-threads=1
        unsafe {
            set_var("XDG_DATA_HOME", "");
            set_var("HOME", "/home/testuser");
        }

        let result = data_dir();
        assert_eq!(result, Some(PathBuf::from("/home/testuser/.local/share")));

        restore_var("XDG_DATA_HOME", original_xdg);
        restore_var("HOME", original_home);
    }

    // #[test]
    // #[cfg(target_os = "linux")]
    // fn linux_cache_dir_ignores_empty_xdg() {
    //     let original_xdg = env::var("XDG_CACHE_HOME").ok();
    //     let original_home = env::var("HOME").ok();
    //     // SAFETY: Tests run single-threaded with --test-threads=1
    //     unsafe {
    //         set_var("XDG_CACHE_HOME", "");
    //         set_var("HOME", "/home/testuser");
    //     }
    //
    //     let result = cache_dir();
    //     assert_eq!(result, Some(PathBuf::from("/home/testuser/.cache")));
    //
    //     restore_var("XDG_CACHE_HOME", original_xdg);
    //     restore_var("HOME", original_home);
    // }
}
