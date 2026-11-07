//! A page's share of the browser's network: which of the schemes the app made privileged are
//! standard (they have an origin) or secure (their pages are secure contexts), as the page's
//! document is told.

use crate::hub::Privileged;

/// The privileged schemes `pick` selects, by name.
pub(crate) fn scheme_names(
    privileged: &[Privileged],
    pick: fn(&crate::hub::Privileges) -> bool,
) -> Vec<String> {
    let chosen = privileged.iter().filter(|p| pick(&p.privileges));
    chosen.map(|p| p.scheme.clone()).collect()
}
