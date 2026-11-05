//! `app.getPath(name)`: the places Electron names, and the ones the app moved with `setPath`.
//!
//! The profile directory is decided once, at start, from the app's name and the command line
//! (`--user-data-dir`), as Electron's `init.ts` does; renaming the app afterwards does not move
//! it. Everything else follows from the system's folders.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::platform::Folders;

/// The places an app has, and the ones it moved.
pub(crate) struct Paths {
    name: String,
    folders: Folders,
    exe: PathBuf,
    user_data: PathBuf,
    moved: BTreeMap<String, PathBuf>,
}

impl Paths {
    /// The places of an app called `name` run from `exe`; `user_data` is `--user-data-dir`.
    pub(crate) fn new(
        folders: Folders,
        exe: PathBuf,
        name: &str,
        user_data: Option<PathBuf>,
    ) -> Self {
        let user_data = user_data.unwrap_or_else(|| folders.app_data.join(name));
        Self {
            name: name.to_owned(),
            folders,
            exe,
            user_data,
            moved: BTreeMap::new(),
        }
    }

    /// Where `name` is; `None` for a name Electron does not know, and for `recent` where the
    /// system has no such place.
    pub(crate) fn get(&self, name: &str) -> Option<PathBuf> {
        if let Some(moved) = self.moved.get(name) {
            return Some(moved.clone());
        }
        let folders = &self.folders;
        let user_data = || {
            self.moved
                .get("userData")
                .unwrap_or(&self.user_data)
                .clone()
        };
        Some(match name {
            "home" => folders.home.clone(),
            "appData" => folders.app_data.clone(),
            "cache" if cfg!(windows) => folders.app_data.clone(),
            "cache" => cache_dir(folders),
            "userData" => self.user_data.clone(),
            "sessionData" => user_data(),
            "temp" => folders.temp.clone(),
            "exe" | "module" => self.exe.clone(),
            "assets" => self.exe.parent()?.to_path_buf(),
            "desktop" => folders.desktop.clone(),
            "documents" => folders.documents.clone(),
            "downloads" => folders.downloads.clone(),
            "music" => folders.music.clone(),
            "pictures" => folders.pictures.clone(),
            "videos" => folders.videos.clone(),
            "recent" => folders.recent.clone()?,
            "logs" if cfg!(target_os = "macos") => {
                folders.home.join("Library/Logs").join(&self.name)
            }
            "logs" => user_data().join("logs"),
            "crashDumps" => user_data().join("Crashpad"),
            _ => return None,
        })
    }

    /// Moves `name` to `path`, as `app.setPath` does.
    pub(crate) fn set(&mut self, name: &str, path: PathBuf) {
        if name == "userData" {
            self.user_data.clone_from(&path);
        }
        self.moved.insert(name.to_owned(), path);
    }

    /// The profile directory.
    pub(crate) fn user_data(&self) -> &Path {
        &self.user_data
    }
}

/// Where a Unix desktop keeps caches.
fn cache_dir(folders: &Folders) -> PathBuf {
    if cfg!(target_os = "macos") {
        folders.home.join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| folders.home.join(".cache"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> Folders {
        let at = |name: &str| PathBuf::from("/h").join(name);
        Folders {
            home: at("home"),
            app_data: at("data"),
            temp: at("tmp"),
            desktop: at("desk"),
            recent: Some(at("recent")),
            ..Folders::default()
        }
    }

    fn paths(user_data: Option<PathBuf>) -> Paths {
        Paths::new(folders(), PathBuf::from("/x/app.exe"), "My App", user_data)
    }

    #[test]
    fn the_profile_is_the_app_data_folder_named_for_the_app_unless_the_command_line_says() {
        assert_eq!(
            paths(None).get("userData"),
            Some(PathBuf::from("/h/data/My App"))
        );
        let given = PathBuf::from("/elsewhere");
        assert_eq!(paths(Some(given.clone())).get("userData"), Some(given));
    }

    #[test]
    fn what_hangs_off_the_profile_follows_it() {
        let mut paths = paths(None);
        assert_eq!(paths.get("sessionData"), paths.get("userData"));
        assert_eq!(
            paths.get("logs"),
            Some(PathBuf::from("/h/data/My App/logs"))
        );
        assert_eq!(
            paths.get("crashDumps"),
            Some(PathBuf::from("/h/data/My App/Crashpad"))
        );
        paths.set("userData", PathBuf::from("/moved"));
        assert_eq!(paths.get("userData"), Some(PathBuf::from("/moved")));
        assert_eq!(paths.get("sessionData"), Some(PathBuf::from("/moved")));
        assert_eq!(
            paths.get("crashDumps"),
            Some(PathBuf::from("/moved/Crashpad"))
        );
    }

    #[test]
    fn the_executable_answers_exe_and_module_and_its_folder_assets() {
        let paths = paths(None);
        assert_eq!(paths.get("exe"), paths.get("module"));
        assert_eq!(paths.get("assets"), Some(PathBuf::from("/x")));
    }

    #[test]
    fn a_name_nobody_knows_has_no_place_and_names_are_case_sensitive() {
        let paths = paths(None);
        assert_eq!(paths.get("bogus"), None);
        assert_eq!(paths.get("appdata"), None);
        assert_eq!(paths.get("temp"), Some(PathBuf::from("/h/tmp")));
    }
}
