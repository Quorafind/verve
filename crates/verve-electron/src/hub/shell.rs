//! The shell's side of the Hub: messages to and from it, the displays it reported and the colour
//! scheme the pages are told.

use std::sync::Arc;

use super::{Hub, lock};
use crate::msg::{AppEvent, Display, FromShell, ThemeSource, ToRendererInput, ToShell};

/// The colour scheme the pages are told: the system's, unless `nativeTheme.themeSource` forces one.
pub(super) struct Theme {
    system_dark: bool,
    source: ThemeSource,
}

impl Theme {
    pub(super) fn new(system_dark: bool) -> Self {
        Self {
            system_dark,
            source: ThemeSource::System,
        }
    }

    /// The system's theme changed to `dark`.
    pub(super) fn set_system(&mut self, dark: bool) {
        self.system_dark = dark;
    }

    pub(super) fn dark(&self) -> bool {
        match self.source {
            ThemeSource::System => self.system_dark,
            ThemeSource::Light => false,
            ThemeSource::Dark => true,
        }
    }
}

/// The shell's end of the Hub: what the OS thread does with what the main process asks of it.
pub trait ShellPort: Send + Sync {
    /// Takes a message. Answers before the Hub wakes the shell.
    fn send(&self, msg: ToShell);

    /// There is news: what was just sent, or a page that published a view. Reached from any
    /// thread, so a shell on another one — GPUI's OS thread — sends itself what it needs.
    fn wake(&self);
}

/// What [`Hub::observe_shell`] calls.
pub(super) type ShellObserver = Arc<dyn Fn(&FromShell) + Send + Sync>;

impl Hub {
    /// Asks the shell for something: a window, a menu, the end of the app.
    pub fn to_shell(&self, msg: ToShell) {
        self.log_to_shell(&msg);
        let shell = lock(&self.shell).clone();
        if let Some(shell) = shell {
            shell.send(msg);
        }
        self.wake();
    }

    /// What the shell saw: a window shown, closed or resized, the displays changed.
    pub fn from_shell(&self, msg: FromShell) {
        self.log_from_shell(&msg);
        if let FromShell::Displays(displays) = &msg {
            self.set_displays(displays.clone());
        }
        let observer = lock(&self.observer).clone();
        if let Some(observer) = observer {
            observer(&msg);
        }
        if let FromShell::App(AppEvent::ThemeChanged { dark }) = &msg {
            lock(&self.theme).set_system(*dark);
        }
        self.to_main(move |guest| guest.from_shell(msg));
    }

    /// Records the displays the shell has: a window the main process makes opens on the primary
    /// one, and `getBounds()` answers where, before the shell has said.
    pub fn set_displays(&self, displays: Vec<Display>) {
        *lock(&self.displays) = displays;
    }

    /// The display new windows open on: the primary one the shell reported, or a 1920 x 1080
    /// screen when it has reported none.
    pub fn primary_display(&self) -> Display {
        let displays = lock(&self.displays);
        let primary = displays.iter().find(|d| d.primary).or(displays.first());
        primary.cloned().unwrap_or_default()
    }

    /// Calls `observer` with every message the shell sends the main process, on the thread that
    /// sends it, before the main process hears it. The same tests watch the headless shell and
    /// the GPUI shell this way; an observer must not block.
    pub fn observe_shell(&self, observer: impl Fn(&FromShell) + Send + Sync + 'static) {
        *lock(&self.observer) = Some(Arc::new(observer));
    }

    /// The displays the shell last reported.
    pub fn displays(&self) -> Vec<Display> {
        lock(&self.displays).clone()
    }

    /// Whether pages are drawn dark: a new page, and the isolate a navigation gives it, starts
    /// with this.
    pub fn dark(&self) -> bool {
        lock(&self.theme).dark()
    }

    /// Whether the system's theme is dark, as the shell last said.
    pub fn system_dark(&self) -> bool {
        lock(&self.theme).system_dark
    }

    /// `nativeTheme.themeSource`.
    pub fn theme_source(&self) -> ThemeSource {
        lock(&self.theme).source
    }

    /// Sets `nativeTheme.themeSource`: the shell and every page are told, and the new
    /// `shouldUseDarkColors` is answered.
    pub fn set_theme_source(&self, source: ThemeSource) -> bool {
        let dark = {
            let mut theme = lock(&self.theme);
            theme.source = source;
            theme.dark()
        };
        self.to_shell(ToShell::SetThemeSource(source));
        let pages: Vec<_> = lock(&self.renderers).keys().copied().collect();
        for contents in pages {
            self.on_renderer(contents, move |guest| {
                guest.input(ToRendererInput::ColorScheme { dark });
            });
        }
        dark
    }

    /// Registers the shell that takes [`ToShell`] messages and the wake that says there are new
    /// ones. A shell that draws on another thread keeps its queue here and is woken there.
    pub fn set_shell(&self, shell: Arc<dyn ShellPort>) {
        *lock(&self.shell) = Some(shell);
    }
}
