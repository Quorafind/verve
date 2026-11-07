//! Reading a file as the app sees it: through its virtual filesystem (so a path inside
//! `app.asar` reads), then from the disk. A page's `file:` loads and `net.fetch` of a `file:` URL
//! share it, and share its failures: Chromium's net errors.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use verve::browser_net::NetError;
use verve::vfs::Overlay;

use crate::msg::{ERR_ACCESS_DENIED, ERR_FAILED, ERR_FILE_NOT_FOUND};

/// The bytes of the file at `path`, or the net error Chromium reports: `ERR_FILE_NOT_FOUND`,
/// `ERR_ACCESS_DENIED`, `ERR_FAILED` (a folder, or anything else).
pub(crate) fn read(overlay: Option<&Arc<dyn Overlay>>, path: &Path) -> Result<Vec<u8>, NetError> {
    let packed = overlay.and_then(|overlay| overlay.read(path));
    let read = match packed {
        Some(read) => read.map(|bytes| bytes.into_owned()),
        None if path.is_dir() => return Err(NetError::from_code(ERR_FAILED)),
        None => std::fs::read(path),
    };
    read.map_err(|error| {
        NetError::from_code(match error.kind() {
            ErrorKind::NotFound => ERR_FILE_NOT_FOUND,
            ErrorKind::PermissionDenied => ERR_ACCESS_DENIED,
            _ => ERR_FAILED,
        })
    })
}

/// When the file at `path` on the disk was last changed, as an HTTP date (`Sat, 07 Nov 2026
/// 03:24:46 GMT`): the `last-modified` of a `file:` answer. `None` for a path that is not on the
/// disk, such as one inside `app.asar`.
pub(crate) fn last_modified(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(http_date(secs))
}

/// `secs` since the Unix epoch, written as RFC 9110 writes a date.
fn http_date(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from the day count (Howard Hinnant's algorithm), the era starting on March 1st.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} GMT",
        DAYS[(days.rem_euclid(7)) as usize],
        MONTHS[(month - 1) as usize],
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_written_as_http_writes_it() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_794_021_886), "Sat, 07 Nov 2026 03:24:46 GMT");
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
    }

    #[test]
    fn a_file_on_the_disk_has_a_date_and_one_that_is_not_there_has_none() {
        let here = std::env::current_exe().unwrap();
        assert!(last_modified(&here).is_some_and(|date| date.ends_with(" GMT")));
        assert_eq!(last_modified(Path::new("/no/such/file")), None);
    }
}
