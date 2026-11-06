//! The menus the shell knows, and which item a pressed key chooses.
//!
//! A [`MenuBook`] follows the [`MenuOp`]s a shell is sent, so it holds the same menus the shell
//! built its native ones from. [`MenuBook::item_for`] is the table of in-window shortcuts: the
//! window's own menu first, then the application menu; off macOS every enabled item that has an
//! accelerator the system is given is in it, whether the item or the menu bar is shown, and the
//! last of two items with one accelerator wins. On macOS an item with `visible: false` is in it
//! only if `acceleratorWorksWhenHidden` (Electron 39's `_shouldCommandIdWorkWhenHidden`, macOS
//! only). The headless shell and the main process
//! (which asks it after `before-input-event`) use it as it is; the GPUI shell must call it on
//! every key down before it sends the key to the page (see `docs/electron/packages/P15`).

use std::collections::BTreeMap;

use super::roles::Os;
use crate::ids::WindowId;
use crate::msg::{MenuId, MenuItemId, MenuItemKind, MenuItemSpec, MenuOp, MenuSpec};

/// The menu bar of one window.
#[derive(Clone, Copy, Debug, Default)]
struct Bar {
    menu: Option<MenuId>,
}

/// Menus by id, the application menu, and each window's own.
#[derive(Clone, Debug)]
pub struct MenuBook {
    os: Os,
    menus: BTreeMap<MenuId, MenuSpec>,
    app: Option<MenuId>,
    bars: BTreeMap<WindowId, Bar>,
}

impl Default for MenuBook {
    fn default() -> Self {
        Self::new(Os::CURRENT)
    }
}

impl MenuBook {
    /// An empty book for a shell running on `os`.
    pub fn new(os: Os) -> Self {
        Self {
            os,
            menus: BTreeMap::new(),
            app: None,
            bars: BTreeMap::new(),
        }
    }

    /// Takes what `op` says of the menus; an op about trays or shortcuts changes nothing here.
    pub fn apply(&mut self, op: &MenuOp) {
        match op {
            MenuOp::Upsert(spec) => {
                self.menus.insert(spec.id, spec.clone());
            }
            MenuOp::Drop(id) => {
                self.menus.remove(id);
            }
            MenuOp::SetApplicationMenu(menu) => self.app = *menu,
            MenuOp::SetWindowMenu { window, menu } => {
                self.bars.entry(*window).or_default().menu = *menu;
            }
            _ => {}
        }
    }

    /// A window is gone.
    pub fn forget_window(&mut self, window: WindowId) {
        self.bars.remove(&window);
    }

    /// The menu `id`, as the shell was last sent it.
    pub fn menu(&self, id: MenuId) -> Option<&MenuSpec> {
        self.menus.get(&id)
    }

    /// The menu a window shows, if any: its own, and on macOS (where the application menu is the
    /// system's) the application menu.
    pub fn menu_of(&self, window: WindowId) -> Option<&MenuSpec> {
        self.in_order(window).into_iter().next()
    }

    fn in_order(&self, window: WindowId) -> Vec<&MenuSpec> {
        let own = self.bars.get(&window);
        let mut order = Vec::new();
        order.extend(
            own.and_then(|bar| bar.menu)
                .and_then(|id| self.menus.get(&id)),
        );
        // Off macOS a window without a menu of its own has none: `removeMenu` removes it.
        if self.os == Os::Mac || own.is_none() {
            order.extend(self.app.and_then(|id| self.menus.get(&id)));
        }
        order
    }

    /// The item `key` chooses in `window`: the last, in window then application menu order, that is
    /// enabled, has an accelerator the system is given and that `key` matches.
    pub fn item_for(&self, window: WindowId, key: &verve_dom::Key) -> Option<MenuItemId> {
        let mac = self.os == Os::Mac;
        self.in_order(window)
            .into_iter()
            .find_map(|menu| find(&menu.items, key, mac))
    }
}

/// The last item of `items` (and of the submenus, whatever the state of the item that opens them)
/// that `key` chooses.
fn find(items: &[MenuItemSpec], key: &verve_dom::Key, mac: bool) -> Option<MenuItemId> {
    let mut found = None;
    for item in items {
        if let MenuItemKind::Submenu(sub) = &item.kind {
            found = find(&sub.items, key, mac).or(found);
            continue;
        }
        let shown = !mac || item.visible || item.accelerator_works_when_hidden;
        let usable = item.enabled
            && item.register_accelerator
            && shown
            && !matches!(item.kind, MenuItemKind::Separator);
        if let Some(accelerator) = item.accelerator.filter(|_| usable)
            && accelerator.matches(key)
        {
            found = Some(item.id);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accelerator::Accelerator;
    use crate::msg::MenuItemSpec;

    fn item(id: MenuItemId, accelerator: Option<&str>) -> MenuItemSpec {
        MenuItemSpec {
            id,
            kind: MenuItemKind::Normal,
            role: None,
            label: format!("item {id}"),
            sublabel: String::new(),
            tool_tip: String::new(),
            accelerator: accelerator.map(|a| Accelerator::parse_for(a, Os::Windows).unwrap()),
            icon: None,
            enabled: true,
            visible: true,
            checked: false,
            register_accelerator: true,
            accelerator_works_when_hidden: true,
        }
    }

    fn menu(id: MenuId, items: Vec<MenuItemSpec>) -> MenuSpec {
        MenuSpec { id, items }
    }

    fn press(code: &str, key: &str, ctrl: bool) -> verve_dom::Key {
        verve_dom::Key {
            key: key.to_owned(),
            code: code.to_owned(),
            modifiers: verve_dom::Modifiers {
                ctrl,
                ..verve_dom::Modifiers::default()
            },
            repeat: false,
        }
    }

    fn book(os: Os) -> MenuBook {
        let mut book = MenuBook::new(os);
        let nested = MenuItemSpec {
            kind: MenuItemKind::Submenu(menu(11, vec![item(2, Some("Ctrl+O")), item(3, None)])),
            ..item(1, None)
        };
        book.apply(&MenuOp::Upsert(menu(10, vec![nested])));
        book.apply(&MenuOp::Upsert(menu(
            20,
            vec![item(4, Some("Ctrl+O")), item(5, Some("Ctrl+K"))],
        )));
        book.apply(&MenuOp::SetApplicationMenu(Some(10)));
        book
    }

    #[test]
    fn an_item_in_a_submenu_is_found_by_its_accelerator() {
        let book = book(Os::Windows);
        let open = press("KeyO", "o", true);
        assert_eq!(book.item_for(1, &open), Some(2));
        assert_eq!(book.item_for(1, &press("KeyO", "o", false)), None);
    }

    #[test]
    fn the_windows_own_menu_comes_before_the_application_menu() {
        let mut book = book(Os::Mac);
        book.apply(&MenuOp::SetWindowMenu {
            window: 1,
            menu: Some(20),
        });
        assert_eq!(book.item_for(1, &press("KeyO", "o", true)), Some(4));
        assert_eq!(book.item_for(2, &press("KeyO", "o", true)), Some(2));
        // Off macOS a window that was given a menu has no other, and one with none has none.
        let mut other = book_for_windows();
        other.apply(&MenuOp::SetWindowMenu {
            window: 1,
            menu: Some(20),
        });
        assert_eq!(other.item_for(1, &press("KeyK", "k", true)), Some(5));
        other.apply(&MenuOp::SetWindowMenu {
            window: 1,
            menu: None,
        });
        assert_eq!(other.item_for(1, &press("KeyO", "o", true)), None);
    }

    fn book_for_windows() -> MenuBook {
        book(Os::Windows)
    }

    #[test]
    fn off_macos_an_invisible_item_keeps_its_accelerator_and_on_macos_only_if_it_works_hidden() {
        let mut spec = menu(30, vec![item(7, Some("Ctrl+H"))]);
        spec.items[0].visible = false;
        spec.items[0].accelerator_works_when_hidden = false;
        let key = press("KeyH", "h", true);
        for (os, found) in [
            (Os::Windows, Some(7)),
            (Os::Linux, Some(7)),
            (Os::Mac, None),
        ] {
            let mut book = MenuBook::new(os);
            book.apply(&MenuOp::Upsert(spec.clone()));
            book.apply(&MenuOp::SetWindowMenu {
                window: 1,
                menu: Some(30),
            });
            assert_eq!(book.item_for(1, &key), found, "{os:?}");
        }
        spec.items[0].accelerator_works_when_hidden = true;
        let mut book = MenuBook::new(Os::Mac);
        book.apply(&MenuOp::Upsert(spec));
        book.apply(&MenuOp::SetWindowMenu {
            window: 1,
            menu: Some(30),
        });
        assert_eq!(book.item_for(1, &key), Some(7));
    }

    #[test]
    fn the_last_of_two_items_with_one_accelerator_wins() {
        let mut book = MenuBook::new(Os::Windows);
        book.apply(&MenuOp::Upsert(menu(
            50,
            vec![item(1, Some("Ctrl+D")), item(2, Some("Ctrl+D"))],
        )));
        book.apply(&MenuOp::SetApplicationMenu(Some(50)));
        assert_eq!(book.item_for(1, &press("KeyD", "d", true)), Some(2));
    }

    #[test]
    fn disabled_and_unregistered_items_do_not_answer_a_key() {
        let mut spec = menu(
            40,
            vec![
                item(1, Some("Ctrl+A")),
                item(2, Some("Ctrl+B")),
                item(3, Some("Ctrl+C")),
            ],
        );
        spec.items[0].enabled = false;
        spec.items[1].visible = false;
        spec.items[2].register_accelerator = false;
        let mut book = MenuBook::new(Os::Windows);
        book.apply(&MenuOp::Upsert(spec));
        book.apply(&MenuOp::SetApplicationMenu(Some(40)));
        for (code, key) in [("KeyA", "a"), ("KeyC", "c")] {
            assert_eq!(book.item_for(1, &press(code, key, true)), None, "{key}");
        }
    }
}
