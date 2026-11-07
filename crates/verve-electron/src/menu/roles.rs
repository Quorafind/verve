//! The roles a menu item can have, with the label, accelerator and submenu each gets by default.
//!
//! Frozen from Electron 39.8.10's own `lib/browser/api/menu-item-roles.ts` as compiled into
//! `electron.exe` (the table `roleList`), and checked against what `new MenuItem({ role })`
//! answers in that build on Windows. A role is spelled as the script wrote it and kept in lower
//! case; a name this table does not know is no role: the item keeps it and gets no default.

/// The system the labels and accelerators are chosen for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    /// The system this program was built for.
    pub const CURRENT: Os = if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    };
}

/// What a role gives an item that does not say otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Defaults {
    /// The label, for an app named `app_name`.
    pub label: &'static str,
    /// Whether the label is followed by the app's name (`About <app>`, `Hide <app>`, `Quit <app>`).
    pub with_app: bool,
    pub accelerator: Option<&'static str>,
    /// Whether the system is given the accelerator; `false` for the roles the page edits by
    /// itself when the key is pressed.
    pub register_accelerator: bool,
    /// Whether the item is a checkbox (`togglespellchecker`).
    pub checkbox: bool,
}

const fn plain(label: &'static str) -> Defaults {
    Defaults {
        label,
        with_app: false,
        accelerator: None,
        register_accelerator: true,
        checkbox: false,
    }
}

const fn keyed(label: &'static str, accelerator: &'static str) -> Defaults {
    Defaults {
        accelerator: Some(accelerator),
        ..plain(label)
    }
}

/// A role whose key the page handles itself, so the system is not given it.
const fn edit(label: &'static str, accelerator: &'static str) -> Defaults {
    Defaults {
        register_accelerator: false,
        ..keyed(label, accelerator)
    }
}

/// The roles that are a label and nothing else.
const LABELS: &[(&str, &str)] = &[
    ("delete", "Delete"),
    ("help", "Help"),
    ("services", "Services"),
    ("recentdocuments", "Open Recent"),
    ("clearrecentdocuments", "Clear Menu"),
    ("showsubstitutions", "Show Substitutions"),
    ("togglesmartquotes", "Smart Quotes"),
    ("togglesmartdashes", "Smart Dashes"),
    ("toggletextreplacement", "Text Replacement"),
    ("startspeaking", "Start Speaking"),
    ("stopspeaking", "Stop Speaking"),
    ("unhide", "Show All"),
    ("window", "Window"),
    ("zoom", "Zoom"),
    ("filemenu", "File"),
    ("editmenu", "Edit"),
    ("viewmenu", "View"),
    ("windowmenu", "Window"),
    ("sharemenu", "Share"),
    ("front", "Bring All to Front"),
];

/// The defaults of `role` (lower case), or `None` for a name that is no role.
pub fn defaults(role: &str, os: Os) -> Option<Defaults> {
    if let Some((_, label)) = LABELS.iter().find(|(name, _)| *name == role) {
        return Some(plain(label));
    }
    let mac = os == Os::Mac;
    let found = match role {
        "about" if os == Os::Linux => plain("About"),
        "about" => Defaults {
            with_app: true,
            ..plain("About ")
        },
        "close" => keyed(
            if mac { "Close Window" } else { "Close" },
            "CommandOrControl+W",
        ),
        "copy" => edit("Copy", "CommandOrControl+C"),
        "cut" => edit("Cut", "CommandOrControl+X"),
        "forcereload" => keyed("Force Reload", "Shift+CmdOrCtrl+R"),
        "hide" => Defaults {
            with_app: true,
            ..keyed("Hide ", "Command+H")
        },
        "hideothers" => keyed("Hide Others", "Command+Alt+H"),
        "minimize" => keyed("Minimize", "CommandOrControl+M"),
        "paste" => edit("Paste", "CommandOrControl+V"),
        "pasteandmatchstyle" => edit(
            "Paste and Match Style",
            if mac {
                "Cmd+Option+Shift+V"
            } else {
                "Shift+CommandOrControl+V"
            },
        ),
        "quit" => quit(os),
        "redo" => keyed(
            "Redo",
            if os == Os::Windows {
                "Control+Y"
            } else {
                "Shift+CommandOrControl+Z"
            },
        ),
        "reload" => keyed("Reload", "CmdOrCtrl+R"),
        "resetzoom" => keyed("Actual Size", "CommandOrControl+0"),
        "selectall" => keyed("Select All", "CommandOrControl+A"),
        "toggledevtools" => keyed(
            "Toggle Developer Tools",
            if mac { "Alt+Command+I" } else { "Ctrl+Shift+I" },
        ),
        "togglefullscreen" => keyed(
            "Toggle Full Screen",
            if mac { "Control+Command+F" } else { "F11" },
        ),
        "undo" => keyed("Undo", "CommandOrControl+Z"),
        "zoomin" => keyed("Zoom In", "CommandOrControl+Plus"),
        "zoomout" => keyed("Zoom Out", "CommandOrControl+-"),
        "togglespellchecker" => Defaults {
            checkbox: true,
            ..plain("Check Spelling While Typing")
        },
        "appmenu" => Defaults {
            with_app: true,
            ..plain("")
        },
        _ => return None,
    };
    Some(found)
}

fn quit(os: Os) -> Defaults {
    match os {
        Os::Mac => Defaults {
            with_app: true,
            ..keyed("Quit ", "CommandOrControl+Q")
        },
        Os::Windows => plain("Exit"),
        Os::Linux => keyed("Quit", "CommandOrControl+Q"),
    }
}

/// The label a role gives, for an app named `app_name`; empty for a name that is no role.
pub fn default_label(role: &str, os: Os, app_name: &str) -> String {
    match defaults(role, os) {
        Some(found) if found.with_app => format!("{}{app_name}", found.label),
        Some(found) => found.label.to_owned(),
        None => String::new(),
    }
}

/// One entry of the submenu a role stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Template {
    Role(&'static str),
    Separator,
    /// An item that is no role: a label and its submenu.
    Menu(&'static str, Vec<Template>),
}

/// The submenu the roles `appMenu`, `fileMenu`, `editMenu`, `viewMenu`, `windowMenu` and
/// `shareMenu` stand for. A role of another name has none (`help` has an empty one only
/// when the script gives it).
pub fn default_submenu(role: &str, os: Os) -> Option<Vec<Template>> {
    use Template::{Role, Separator};
    let mac = os == Os::Mac;
    let items = match role {
        "appmenu" => vec![
            Role("about"),
            Separator,
            Role("services"),
            Separator,
            Role("hide"),
            Role("hideOthers"),
            Role("unhide"),
            Separator,
            Role("quit"),
        ],
        "filemenu" => vec![Role(if mac { "close" } else { "quit" })],
        "editmenu" => edit_menu(mac),
        "viewmenu" => vec![
            Role("reload"),
            Role("forceReload"),
            Role("toggleDevTools"),
            Separator,
            Role("resetZoom"),
            Role("zoomIn"),
            Role("zoomOut"),
            Separator,
            Role("togglefullscreen"),
        ],
        "windowmenu" => {
            let mut items = vec![Role("minimize"), Role("zoom")];
            if mac {
                items.extend([Separator, Role("front")]);
            } else {
                items.push(Role("close"));
            }
            items
        }
        "sharemenu" => Vec::new(),
        _ => return None,
    };
    Some(items)
}

fn edit_menu(mac: bool) -> Vec<Template> {
    use Template::{Menu, Role, Separator};
    let mut items = vec![
        Role("undo"),
        Role("redo"),
        Separator,
        Role("cut"),
        Role("copy"),
        Role("paste"),
    ];
    if mac {
        items.extend([
            Role("pasteAndMatchStyle"),
            Role("delete"),
            Role("selectAll"),
            Separator,
            Menu(
                "Substitutions",
                vec![
                    Role("showSubstitutions"),
                    Separator,
                    Role("toggleSmartQuotes"),
                    Role("toggleSmartDashes"),
                    Role("toggleTextReplacement"),
                ],
            ),
            Menu("Speech", vec![Role("startSpeaking"), Role("stopSpeaking")]),
        ]);
    } else {
        items.extend([Role("delete"), Separator, Role("selectAll")]);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_accelerators_are_those_electron_39_answers_on_windows() {
        let win = |role| defaults(role, Os::Windows).expect(role);
        assert_eq!(win("undo").accelerator, Some("CommandOrControl+Z"));
        assert_eq!(win("redo").accelerator, Some("Control+Y"));
        assert_eq!(win("quit").label, "Exit");
        assert_eq!(win("quit").accelerator, None);
        assert_eq!(win("toggledevtools").accelerator, Some("Ctrl+Shift+I"));
        assert_eq!(win("togglefullscreen").accelerator, Some("F11"));
        assert_eq!(win("zoomin").accelerator, Some("CommandOrControl+Plus"));
        assert_eq!(win("zoomout").accelerator, Some("CommandOrControl+-"));
        assert_eq!(
            win("pasteandmatchstyle").accelerator,
            Some("Shift+CommandOrControl+V")
        );
        assert_eq!(win("forcereload").accelerator, Some("Shift+CmdOrCtrl+R"));
        assert_eq!(win("reload").accelerator, Some("CmdOrCtrl+R"));
        assert_eq!(win("delete").accelerator, None);
        assert_eq!(win("unhide").label, "Show All");
        assert_eq!(win("front").label, "Bring All to Front");
        assert_eq!(
            win("togglespellchecker").label,
            "Check Spelling While Typing"
        );
        assert!(win("togglespellchecker").checkbox);
    }

    #[test]
    fn only_the_edit_roles_leave_their_accelerator_to_the_page() {
        let unregistered: Vec<_> = [
            "cut",
            "copy",
            "paste",
            "pasteandmatchstyle",
            "undo",
            "redo",
            "selectall",
            "delete",
        ]
        .into_iter()
        .filter(|role| !defaults(role, Os::Windows).unwrap().register_accelerator)
        .collect();
        assert_eq!(unregistered, ["cut", "copy", "paste", "pasteandmatchstyle"]);
    }

    #[test]
    fn the_system_changes_a_label_an_accelerator_and_a_submenu() {
        assert_eq!(default_label("quit", Os::Mac, "App"), "Quit App");
        assert_eq!(default_label("quit", Os::Windows, "App"), "Exit");
        assert_eq!(default_label("quit", Os::Linux, "App"), "Quit");
        assert_eq!(default_label("about", Os::Linux, "App"), "About");
        assert_eq!(default_label("about", Os::Windows, "App"), "About App");
        assert_eq!(default_label("hide", Os::Windows, "App"), "Hide App");
        assert_eq!(default_label("appmenu", Os::Mac, "App"), "App");
        assert_eq!(default_label("close", Os::Mac, "App"), "Close Window");
        assert_eq!(default_label("bogus", Os::Windows, "App"), "");
        assert_eq!(
            defaults("redo", Os::Linux).unwrap().accelerator,
            Some("Shift+CommandOrControl+Z")
        );
        assert_eq!(
            defaults("togglefullscreen", Os::Mac).unwrap().accelerator,
            Some("Control+Command+F")
        );
    }

    #[test]
    fn the_role_menus_expand_as_electron_expands_them() {
        use Template::{Role, Separator};
        assert_eq!(
            default_submenu("editmenu", Os::Windows).unwrap(),
            [
                Role("undo"),
                Role("redo"),
                Separator,
                Role("cut"),
                Role("copy"),
                Role("paste"),
                Role("delete"),
                Separator,
                Role("selectAll"),
            ]
        );
        assert_eq!(
            default_submenu("filemenu", Os::Windows).unwrap(),
            [Role("quit")]
        );
        assert_eq!(
            default_submenu("filemenu", Os::Mac).unwrap(),
            [Role("close")]
        );
        assert_eq!(
            default_submenu("windowmenu", Os::Windows).unwrap(),
            [Role("minimize"), Role("zoom"), Role("close")]
        );
        assert_eq!(
            default_submenu("windowmenu", Os::Mac).unwrap(),
            [Role("minimize"), Role("zoom"), Separator, Role("front")]
        );
        assert_eq!(default_submenu("viewmenu", Os::Linux).unwrap().len(), 9);
        assert_eq!(default_submenu("appmenu", Os::Mac).unwrap().len(), 9);
        assert_eq!(default_submenu("sharemenu", Os::Mac).unwrap(), []);
        assert_eq!(default_submenu("help", Os::Windows), None);
        assert_eq!(default_submenu("copy", Os::Windows), None);
        let mac_edit = default_submenu("editmenu", Os::Mac).unwrap();
        assert_eq!(mac_edit.len(), 12);
        assert!(
            matches!(mac_edit.last(), Some(Template::Menu("Speech", items)) if items.len() == 2)
        );
    }
}
