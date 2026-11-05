//! What `app` asks of the operating system: the folders it names, the user's language, memory
//! and CPU figures, the registry keys of a protocol client and of login items.
//!
//! Windows is the platform written first; elsewhere the folders come from the home directory
//! and the XDG variables, and the registry is not there to ask.

use std::path::PathBuf;

/// The places the system names, which `app.getPath` answers from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Folders {
    pub(crate) home: PathBuf,
    pub(crate) app_data: PathBuf,
    pub(crate) temp: PathBuf,
    pub(crate) desktop: PathBuf,
    pub(crate) documents: PathBuf,
    pub(crate) downloads: PathBuf,
    pub(crate) music: PathBuf,
    pub(crate) pictures: PathBuf,
    pub(crate) videos: PathBuf,
    /// `recent` exists on Windows only.
    pub(crate) recent: Option<PathBuf>,
}

/// Physical and swap memory, in kilobytes, as `process.getSystemMemoryInfo()` has them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SystemMemory {
    pub(crate) total: u64,
    pub(crate) free: u64,
    pub(crate) swap_total: u64,
    pub(crate) swap_free: u64,
}

/// What this process holds of memory, in kilobytes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ProcessMemory {
    pub(crate) resident: u64,
    pub(crate) private: u64,
    pub(crate) peak_resident: u64,
}

#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(windows))]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Globalization::{
        GetUserDefaultLocaleName, GetUserPreferredUILanguages, MUI_LANGUAGE_NAME,
    };
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_SZ, RRF_RT_REG_DWORD,
        RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteKeyW,
        RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Recent, FOLDERID_RoamingAppData,
        FOLDERID_Videos, SHGetKnownFolderPath, SetCurrentProcessExplicitAppUserModelID,
    };

    use super::{Folders, ProcessMemory, SystemMemory};

    /// A NUL-terminated UTF-16 copy of `text`, for the wide APIs.
    pub(crate) fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    fn known_folder(id: &windows_sys::core::GUID) -> PathBuf {
        let mut path = std::ptr::null_mut();
        // SAFETY: `path` is an out-pointer the call fills with a NUL-terminated string it
        // allocates, which is copied and then freed with `CoTaskMemFree` as the API requires.
        unsafe {
            let status = SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut path);
            if status < 0 || path.is_null() {
                return PathBuf::new();
            }
            let length = (0..).take_while(|&i| *path.add(i) != 0).count();
            let found = std::ffi::OsString::from_wide(std::slice::from_raw_parts(path, length));
            CoTaskMemFree(path.cast());
            found.into()
        }
    }

    pub(crate) fn folders() -> Folders {
        Folders {
            home: known_folder(&FOLDERID_Profile),
            app_data: known_folder(&FOLDERID_RoamingAppData),
            temp: std::env::temp_dir(),
            desktop: known_folder(&FOLDERID_Desktop),
            documents: known_folder(&FOLDERID_Documents),
            downloads: known_folder(&FOLDERID_Downloads),
            music: known_folder(&FOLDERID_Music),
            pictures: known_folder(&FOLDERID_Pictures),
            videos: known_folder(&FOLDERID_Videos),
            recent: Some(known_folder(&FOLDERID_Recent)),
        }
    }

    /// The user's regional format, such as `zh-SG`.
    pub(crate) fn system_locale() -> String {
        let mut name = [0u16; 85];
        // SAFETY: the buffer is the 85 units `LOCALE_NAME_MAX_LENGTH` allows.
        let length = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), 85) };
        let end = usize::try_from(length).unwrap_or(0).saturating_sub(1);
        String::from_utf16_lossy(&name[..end])
    }

    /// The display languages in the order the user prefers them.
    pub(crate) fn preferred_languages() -> Vec<String> {
        let (mut count, mut length) = (0u32, 0u32);
        // SAFETY: the first call only measures; the second fills a buffer of that many units.
        unsafe {
            let measure = GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                std::ptr::null_mut(),
                &mut length,
            );
            if measure == 0 || length == 0 {
                return Vec::new();
            }
            let mut buffer = vec![0u16; length as usize];
            let read = GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                buffer.as_mut_ptr(),
                &mut length,
            );
            if read == 0 {
                return Vec::new();
            }
            buffer
                .split(|&unit| unit == 0)
                .filter(|name| !name.is_empty())
                .map(String::from_utf16_lossy)
                .collect()
        }
    }

    pub(crate) fn system_memory() -> SystemMemory {
        // SAFETY: `status` is initialised with the size the call needs.
        let status = unsafe {
            let mut status: MEMORYSTATUSEX = std::mem::zeroed();
            status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            GlobalMemoryStatusEx(&mut status);
            status
        };
        SystemMemory {
            total: status.ullTotalPhys / 1024,
            free: status.ullAvailPhys / 1024,
            swap_total: status.ullTotalPageFile / 1024,
            swap_free: status.ullAvailPageFile / 1024,
        }
    }

    pub(crate) fn process_memory() -> ProcessMemory {
        // SAFETY: `counters` is initialised with the size the call needs.
        let counters = unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                counters.cb,
            );
            counters
        };
        ProcessMemory {
            resident: counters.WorkingSetSize as u64 / 1024,
            private: counters.PrivateUsage as u64 / 1024,
            peak_resident: counters.PeakWorkingSetSize as u64 / 1024,
        }
    }

    /// The seconds of CPU this process has used, kernel and user.
    pub(crate) fn cpu_seconds() -> f64 {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: four out-pointers to FILETIMEs.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            );
        }
        let ticks =
            |t: FILETIME| (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) as f64;
        (ticks(kernel) + ticks(user)) / 1e7
    }

    /// Windows' own version, `10.0.19044`, from the registry the way the system reports it.
    pub(crate) fn system_version() -> String {
        let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let number = |name: &str| read_dword(Root::LocalMachine, key, name).unwrap_or(0);
        let build = read_string(Root::LocalMachine, key, "CurrentBuildNumber").unwrap_or_default();
        format!(
            "{}.{}.{build}",
            number("CurrentMajorVersionNumber"),
            number("CurrentMinorVersionNumber")
        )
    }

    pub(crate) fn set_app_user_model_id(id: &str) {
        // SAFETY: a NUL-terminated string that outlives the call.
        unsafe {
            SetCurrentProcessExplicitAppUserModelID(wide(id).as_ptr());
        }
    }

    /// The part of the registry the app reads and writes.
    #[derive(Clone, Copy)]
    pub(crate) enum Root {
        CurrentUser,
        LocalMachine,
    }

    impl Root {
        fn key(self) -> HKEY {
            match self {
                Root::CurrentUser => HKEY_CURRENT_USER,
                Root::LocalMachine => HKEY_LOCAL_MACHINE,
            }
        }
    }

    pub(crate) fn read_string(root: Root, key: &str, name: &str) -> Option<String> {
        let (key, name) = (wide(key), wide(name));
        let mut bytes = 0u32;
        // SAFETY: the first call measures the value, the second fills a buffer of that size.
        unsafe {
            let flags = RRF_RT_REG_SZ;
            if RegGetValueW(
                root.key(),
                key.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            ) != 0
            {
                return None;
            }
            let mut buffer = vec![0u16; bytes as usize / 2 + 1];
            let status = RegGetValueW(
                root.key(),
                key.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            );
            let end = buffer
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(buffer.len());
            (status == 0).then(|| String::from_utf16_lossy(&buffer[..end]))
        }
    }

    fn read_dword(root: Root, key: &str, name: &str) -> Option<u32> {
        let (key, name) = (wide(key), wide(name));
        let (mut value, mut bytes) = (0u32, 4u32);
        // SAFETY: `value` holds the four bytes a DWORD needs.
        let status = unsafe {
            RegGetValueW(
                root.key(),
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut bytes,
            )
        };
        (status == 0).then_some(value)
    }

    /// Sets `name` under `key` to `value`, making the key; the default value is `""`.
    pub(crate) fn write_string(root: Root, key: &str, name: &str, value: &str) -> bool {
        let (key, name, data) = (wide(key), wide(name), wide(value));
        let mut opened: HKEY = std::ptr::null_mut();
        // SAFETY: `data` outlives the call, and the key opened is closed.
        unsafe {
            let made = RegCreateKeyExW(
                root.key(),
                key.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_READ | KEY_WRITE,
                std::ptr::null(),
                &mut opened,
                std::ptr::null_mut(),
            );
            if made != 0 {
                return false;
            }
            let name = if name.len() == 1 {
                std::ptr::null()
            } else {
                name.as_ptr()
            };
            let set = RegSetValueExW(
                opened,
                name,
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            );
            RegCloseKey(opened);
            set == 0
        }
    }

    pub(crate) fn delete_tree(root: Root, key: &str) -> bool {
        // SAFETY: a NUL-terminated key name.
        unsafe { RegDeleteTreeW(root.key(), wide(key).as_ptr()) == 0 }
    }

    /// Whether `key` has neither values nor subkeys. A key that is not there is not empty.
    pub(crate) fn is_key_empty(root: Root, key: &str) -> bool {
        let mut opened: HKEY = std::ptr::null_mut();
        // SAFETY: the key opened is closed; the counts are out-pointers the call fills.
        unsafe {
            if RegOpenKeyExW(root.key(), wide(key).as_ptr(), 0, KEY_READ, &mut opened) != 0 {
                return false;
            }
            let (mut subkeys, mut values) = (0u32, 0u32);
            let status = RegQueryInfoKeyW(
                opened,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut subkeys,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut values,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            RegCloseKey(opened);
            status == 0 && subkeys == 0 && values == 0
        }
    }

    /// Deletes `key`, which must have no subkeys.
    pub(crate) fn delete_key(root: Root, key: &str) -> bool {
        // SAFETY: a NUL-terminated key name.
        unsafe { RegDeleteKeyW(root.key(), wide(key).as_ptr()) == 0 }
    }

    pub(crate) fn delete_value(root: Root, key: &str, name: &str) -> bool {
        // SAFETY: NUL-terminated names.
        unsafe { RegDeleteKeyValueW(root.key(), wide(key).as_ptr(), wide(name).as_ptr()) == 0 }
    }
}

#[cfg(not(windows))]
mod unix {
    use std::path::PathBuf;

    use super::{Folders, ProcessMemory, SystemMemory};

    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    }

    /// `name` from `~/.config/user-dirs.dirs` (`XDG_DOWNLOAD_DIR="$HOME/Downloads"`), or the
    /// folder of that name in the home directory.
    fn user_dir(config: &str, key: &str, default: &str) -> PathBuf {
        let home = home();
        let wanted = format!("XDG_{key}_DIR=");
        config
            .lines()
            .find_map(|line| line.trim().strip_prefix(&wanted))
            .map(|value| {
                value
                    .trim_matches('"')
                    .replace("$HOME", &home.to_string_lossy())
            })
            .map_or_else(|| home.join(default), PathBuf::from)
    }

    pub(crate) fn folders() -> Folders {
        let home = home();
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
        let mac = cfg!(target_os = "macos");
        Folders {
            app_data: if mac {
                home.join("Library/Application Support")
            } else {
                config
            },
            temp: std::env::temp_dir(),
            desktop: user_dir(&dirs, "DESKTOP", "Desktop"),
            documents: user_dir(&dirs, "DOCUMENTS", "Documents"),
            downloads: user_dir(&dirs, "DOWNLOAD", "Downloads"),
            music: user_dir(&dirs, "MUSIC", "Music"),
            pictures: user_dir(&dirs, "PICTURES", "Pictures"),
            videos: user_dir(&dirs, "VIDEOS", "Videos"),
            recent: None,
            home,
        }
    }

    /// `LANG` read as a BCP 47 tag: `de_DE.UTF-8` is `de-DE`.
    pub(crate) fn system_locale() -> String {
        let lang = std::env::var("LANG").unwrap_or_default();
        let tag = lang.split(['.', '@']).next().unwrap_or("");
        if tag.is_empty() || tag == "C" || tag == "POSIX" {
            "en-US".into()
        } else {
            tag.replace('_', "-")
        }
    }

    pub(crate) fn preferred_languages() -> Vec<String> {
        vec![system_locale()]
    }

    /// One `/proc/meminfo` line in kilobytes.
    fn meminfo(text: &str, key: &str) -> u64 {
        text.lines()
            .find_map(|line| {
                line.strip_prefix(key)?
                    .trim_start_matches(':')
                    .split_whitespace()
                    .next()
            })
            .and_then(|kb| kb.parse().ok())
            .unwrap_or(0)
    }

    pub(crate) fn system_memory() -> SystemMemory {
        let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        SystemMemory {
            total: meminfo(&text, "MemTotal"),
            free: meminfo(&text, "MemAvailable"),
            swap_total: meminfo(&text, "SwapTotal"),
            swap_free: meminfo(&text, "SwapFree"),
        }
    }

    pub(crate) fn process_memory() -> ProcessMemory {
        let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let resident = meminfo(&text, "VmRSS");
        ProcessMemory {
            resident,
            private: meminfo(&text, "VmData"),
            peak_resident: meminfo(&text, "VmHWM").max(resident),
        }
    }

    /// The seconds of CPU this process has used, from `/proc/self/stat` (user and system ticks
    /// of 10 ms).
    pub(crate) fn cpu_seconds() -> f64 {
        let text = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let after_name = text.rsplit_once(')').map_or("", |(_, rest)| rest);
        let ticks = |at: usize| {
            after_name
                .split_whitespace()
                .nth(at)
                .and_then(|t| t.parse::<f64>().ok())
                .unwrap_or(0.)
        };
        (ticks(11) + ticks(12)) / 100.
    }

    pub(crate) fn system_version() -> String {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.trim().to_owned())
            .unwrap_or_default()
    }
}

/// The directory `path` names, made if it is not there: what a profile directory is at `ready`.
pub(crate) fn make_dir(path: &std::path::Path) {
    // A directory that cannot be made shows when something is written in it.
    let _ = std::fs::create_dir_all(path);
}
