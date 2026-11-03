//! Sweeps the code cache directory: deletes the packs and marks other builds (other Verve versions,
//! other recompiles) left and nothing has used for long, then, if the directory is still too large,
//! deletes the least recently used until it is under the limit. The current build's pack is never
//! deleted.
//!
//! A sweep runs only at the **end** of a run, after the first flush, and at most once a day: it
//! reads the directory and deletes files, which the startup path must not do. Any failure is skipped
//! and leaves the exit code alone. P42 uses it to clear the old generation directories Electron apps
//! leave; it takes only a directory and the current file, never a `Runtime`.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// The file in the cache directory that records when the last sweep was.
const THROTTLE: &str = "last-sweep";

/// What a sweep follows: how long unused before a file goes, how large the directory may be, and how
/// long apart sweeps are.
#[derive(Clone, Copy)]
pub(crate) struct Rules {
    pub(crate) max_age: Duration,
    pub(crate) max_total: u64,
    pub(crate) every: Duration,
}

/// The default rules: unused for two weeks, a file goes; over 512 MB, the least recently used go;
/// one sweep a day at most.
pub(crate) const RULES: Rules = Rules {
    max_age: Duration::from_secs(14 * 24 * 60 * 60),
    max_total: 512 << 20,
    every: Duration::from_secs(24 * 60 * 60),
};

/// Deletes the other builds' packs and marks in `dir`: those untouched for [`Rules::max_age`] go
/// first, then, while the directory is over [`Rules::max_total`], the oldest by modification time.
/// `current` is this run's pack and is never deleted, nor is the mark of the same name. `now` is the
/// present time.
pub(crate) fn sweep(dir: &Path, current: &Path, now: SystemTime) {
    sweep_with(dir, current, now, RULES);
}

fn sweep_with(dir: &Path, current: &Path, now: SystemTime, rules: Rules) {
    if !due(dir, now, rules.every) {
        return;
    }
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    let current = current.file_name();
    let stem = current.and_then(|name| Path::new(name).file_stem().map(|stem| stem.to_owned()));
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = listing
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let extension = Path::new(&name).extension()?.to_owned();
            if extension != "pack" && extension != "seen" {
                return None;
            }
            if current == Some(name.as_os_str()) || Path::new(&name).file_stem() == stem.as_deref()
            {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Some((entry.path(), used, metadata.len()))
        })
        .collect();
    // Oldest first: deletion by age and by size both follow this order.
    entries.sort_by_key(|(_, used, _)| *used);
    let mut total = entries.iter().map(|(_, _, len)| len).sum::<u64>();
    entries.retain(|(path, used, len)| {
        let stale = now
            .duration_since(*used)
            .is_ok_and(|unused| unused > rules.max_age);
        if stale && fs::remove_file(path).is_ok() {
            total -= len;
            return false;
        }
        !stale
    });
    for (path, _, len) in &entries {
        if total <= rules.max_total {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total -= len;
        }
    }
    let _ = fs::write(dir.join(THROTTLE), seconds(now).to_string());
}

/// Whether enough time has passed since the last sweep; none ever made, or a record that cannot be
/// read, counts as enough.
fn due(dir: &Path, now: SystemTime, every: Duration) -> bool {
    let Ok(text) = fs::read_to_string(dir.join(THROTTLE)) else {
        return true;
    };
    let Ok(then) = text.trim().parse::<u64>() else {
        return true;
    };
    seconds(now).saturating_sub(then) >= every.as_secs()
}

fn seconds(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "verve-code-cache-sweep-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        /// `name`: `len` bytes, last modified `age` ago.
        fn file(&self, name: &str, len: usize, age: Duration) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, vec![0u8; len]).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - age)
                .unwrap();
            path
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const HOUR: Duration = Duration::from_secs(60 * 60);
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const CURRENT: &str = "0000000000000000.pack";

    fn sweep_now(fixture: &Fixture, rules: Rules) {
        sweep_with(
            &fixture.0,
            &fixture.0.join(CURRENT),
            SystemTime::now(),
            rules,
        );
    }

    #[test]
    fn packs_unused_for_a_fortnight_go_and_the_current_one_stays() {
        let fixture = Fixture::new("age");
        fixture.file(CURRENT, 10, 30 * DAY);
        fixture.file("1111111111111111.pack", 10, 30 * DAY);
        fixture.file("1111111111111111.seen", 10, 30 * DAY);
        fixture.file("2222222222222222.pack", 10, 13 * DAY);
        fixture.file("2222222222222222.seen", 10, 13 * DAY);
        sweep_now(&fixture, RULES);
        assert_eq!(
            fixture.names(),
            [
                CURRENT,
                "2222222222222222.pack",
                "2222222222222222.seen",
                THROTTLE
            ]
        );
    }

    #[test]
    fn a_directory_over_the_size_cap_is_emptied_from_the_least_recently_used() {
        let fixture = Fixture::new("size");
        // 900 bytes of other builds' packs against a 500-byte cap: one deletion leaves 600, which
        // is still over, so the second goes too.
        let rules = Rules {
            max_total: 500,
            ..RULES
        };
        fixture.file(CURRENT, 100, 30 * DAY);
        let older = fixture.file("2222222222222222.pack", 300, 30 * HOUR);
        let old = fixture.file("1111111111111111.pack", 300, 20 * HOUR);
        fixture.file("3333333333333333.pack", 300, 10 * HOUR);
        sweep_now(&fixture, rules);
        assert_eq!(
            fixture.names(),
            [CURRENT, "3333333333333333.pack", THROTTLE],
            "the two least recently used went: {older:?} {old:?}"
        );
    }

    #[test]
    fn a_directory_is_looked_at_once_a_day_at_most() {
        let fixture = Fixture::new("throttle");
        let started = SystemTime::now();
        fixture.file(CURRENT, 10, 30 * DAY);
        fixture.file("1111111111111111.pack", 10, 30 * DAY);
        sweep_with(&fixture.0, &fixture.0.join(CURRENT), started, RULES);
        assert!(!fixture.0.join("1111111111111111.pack").exists());
        fixture.file("2222222222222222.pack", 10, 30 * DAY);
        sweep_with(&fixture.0, &fixture.0.join(CURRENT), started + HOUR, RULES);
        assert!(
            fixture.0.join("2222222222222222.pack").exists(),
            "a second sweep within the day deleted a pack"
        );
        sweep_with(
            &fixture.0,
            &fixture.0.join(CURRENT),
            started + 2 * DAY,
            RULES,
        );
        assert!(!fixture.0.join("2222222222222222.pack").exists());
    }

    #[test]
    fn a_throttle_file_that_makes_no_sense_is_no_reason_not_to_sweep() {
        let fixture = Fixture::new("throttle-damaged");
        fs::write(fixture.0.join(THROTTLE), b"yesterday").unwrap();
        fixture.file(CURRENT, 10, 30 * DAY);
        fixture.file("1111111111111111.pack", 10, 30 * DAY);
        sweep_now(&fixture, RULES);
        assert!(!fixture.0.join("1111111111111111.pack").exists());
    }

    #[test]
    fn a_directory_that_is_not_there_is_no_error() {
        let fixture = Fixture::new("missing");
        let nowhere = fixture.0.join("nowhere");
        sweep_with(&nowhere, &nowhere.join(CURRENT), SystemTime::now(), RULES);
        assert!(!nowhere.exists());
    }
}
