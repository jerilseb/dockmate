//! The single source of truth for what dockyard can do.
//!
//! Key handling, the footer key-bar, the `?` help sheet and the `:` command
//! palette all read from [`COMMANDS`], so a binding can never appear in one and
//! be missing from another.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    // Navigation
    NextTab,
    PrevTab,
    TabContainers,
    TabImages,
    TabVolumes,
    TabNetworks,
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,

    // Grouping
    ToggleGroup,
    ToggleCollapse,
    ToggleCollapseAll,

    // Panes
    ToggleDetail,
    ToggleLogs,
    FocusNext,
    ToggleFollow,
    ToggleWrap,
    ToggleTimestamps,

    // Container lifecycle
    Start,
    Stop,
    Restart,
    Pause,
    Kill,
    Remove,
    Exec,

    // Bulk
    Prune,

    // Misc
    CopyId,
    Filter,
    ClearFilter,
    Palette,
    Help,
    Refresh,
    ToggleAll,
    SortNext,
    SortReverse,
    Quit,
}

/// Which tab(s) a command applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Available everywhere.
    Global,
    /// Only on the containers tab.
    Containers,
    /// On any tab that lists removable resources.
    Removable,
}

/// A key, reduced to what we actually discriminate on. Shift is already encoded
/// in the character for printable keys, so only Ctrl needs its own flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
}

impl Key {
    pub const fn c(ch: char) -> Self {
        Self {
            code: KeyCode::Char(ch),
            ctrl: false,
        }
    }
    pub const fn ctrl(ch: char) -> Self {
        Self {
            code: KeyCode::Char(ch),
            ctrl: true,
        }
    }
    pub const fn code(code: KeyCode) -> Self {
        Self { code, ctrl: false }
    }

    fn matches(&self, ev: &KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        self.ctrl == ctrl && self.code == ev.code
    }

    /// How the key is written in the UI.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "⏎".to_string(),
            KeyCode::Esc => "esc".to_string(),
            KeyCode::Tab => "tab".to_string(),
            KeyCode::BackTab => "⇧tab".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::PageUp => "pgup".to_string(),
            KeyCode::PageDown => "pgdn".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            other => format!("{other:?}").to_lowercase(),
        };
        if self.ctrl { format!("^{base}") } else { base }
    }
}

pub struct CommandSpec {
    pub command: Command,
    pub keys: &'static [Key],
    /// Palette entry, e.g. `container: restart`.
    pub name: &'static str,
    pub help: &'static str,
    pub scope: Scope,
    /// Shown in the footer key-bar when true. The footer only has room for the
    /// handful of things people reach for constantly.
    pub footer: bool,
}

pub const COMMANDS: &[CommandSpec] = &[
    // ---- navigation ------------------------------------------------------
    CommandSpec {
        command: Command::NextTab,
        keys: &[Key::code(KeyCode::Tab)],
        name: "go: next tab",
        help: "Move to the next tab",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::PrevTab,
        keys: &[Key::code(KeyCode::BackTab)],
        name: "go: previous tab",
        help: "Move to the previous tab",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::TabContainers,
        keys: &[Key::c('1')],
        name: "go: containers",
        help: "Show the containers tab",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::TabImages,
        keys: &[Key::c('2')],
        name: "go: images",
        help: "Show the images tab",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::TabVolumes,
        keys: &[Key::c('3')],
        name: "go: volumes",
        help: "Show the volumes tab",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::TabNetworks,
        keys: &[Key::c('4')],
        name: "go: networks",
        help: "Show the networks tab",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Down,
        keys: &[Key::code(KeyCode::Down), Key::c('j')],
        name: "go: down",
        help: "Move down one row",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Up,
        keys: &[Key::code(KeyCode::Up), Key::c('k')],
        name: "go: up",
        help: "Move up one row",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::PageDown,
        keys: &[Key::code(KeyCode::PageDown), Key::ctrl('d')],
        name: "go: page down",
        help: "Move down a page",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::PageUp,
        keys: &[Key::code(KeyCode::PageUp), Key::ctrl('u')],
        name: "go: page up",
        help: "Move up a page",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Top,
        keys: &[Key::c('g'), Key::code(KeyCode::Home)],
        name: "go: first",
        help: "Jump to the first row",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Bottom,
        keys: &[Key::c('G'), Key::code(KeyCode::End)],
        name: "go: last",
        help: "Jump to the last row",
        scope: Scope::Global,
        footer: false,
    },
    // ---- grouping --------------------------------------------------------
    CommandSpec {
        command: Command::ToggleGroup,
        keys: &[Key::c('z')],
        name: "group by stack",
        help: "Group containers by their compose project or swarm stack",
        scope: Scope::Containers,
        footer: true,
    },
    CommandSpec {
        command: Command::ToggleCollapse,
        keys: &[Key::c(' ')],
        name: "stack: fold",
        help: "Fold or unfold the stack the cursor is in",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::ToggleCollapseAll,
        keys: &[Key::c('Z')],
        name: "stack: fold all",
        help: "Fold every stack, or unfold them all if none are folded",
        scope: Scope::Containers,
        footer: false,
    },
    // ---- panes -----------------------------------------------------------
    CommandSpec {
        command: Command::ToggleDetail,
        keys: &[Key::code(KeyCode::Enter)],
        name: "view: toggle details",
        help: "Show or hide the detail pane",
        scope: Scope::Global,
        footer: true,
    },
    CommandSpec {
        command: Command::ToggleLogs,
        keys: &[Key::c('l')],
        name: "view: toggle logs",
        help: "Show or hide the log pane",
        scope: Scope::Containers,
        footer: true,
    },
    CommandSpec {
        command: Command::FocusNext,
        keys: &[Key::ctrl('w')],
        name: "view: switch pane focus",
        help: "Move focus between the list and the log pane",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::ToggleFollow,
        keys: &[Key::c('f')],
        name: "logs: toggle follow",
        help: "Follow new log lines, or stay put",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::ToggleWrap,
        keys: &[Key::c('w')],
        name: "logs: toggle wrap",
        help: "Wrap long log lines",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::ToggleTimestamps,
        keys: &[Key::c('t')],
        name: "logs: toggle timestamps",
        help: "Show or hide log timestamps",
        scope: Scope::Containers,
        footer: false,
    },
    // ---- lifecycle -------------------------------------------------------
    CommandSpec {
        command: Command::Start,
        keys: &[Key::c('u')],
        name: "container: start",
        help: "Start the selected container",
        scope: Scope::Containers,
        footer: true,
    },
    CommandSpec {
        command: Command::Stop,
        keys: &[Key::c('S')],
        name: "container: stop",
        help: "Stop the selected container",
        scope: Scope::Containers,
        footer: true,
    },
    CommandSpec {
        command: Command::Restart,
        keys: &[Key::c('r')],
        name: "container: restart",
        help: "Restart the selected container",
        scope: Scope::Containers,
        footer: true,
    },
    CommandSpec {
        command: Command::Pause,
        keys: &[Key::c('p')],
        name: "container: pause / unpause",
        help: "Freeze or resume the selected container",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::Kill,
        keys: &[Key::c('K')],
        name: "container: kill",
        help: "Send SIGKILL to the selected container",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::Exec,
        keys: &[Key::c('s')],
        name: "container: shell",
        help: "Open an interactive shell inside the container",
        scope: Scope::Containers,
        footer: true,
    },
    CommandSpec {
        command: Command::Remove,
        keys: &[Key::c('d')],
        name: "remove selected",
        help: "Delete the selected resource (asks first)",
        scope: Scope::Removable,
        footer: true,
    },
    CommandSpec {
        command: Command::Prune,
        keys: &[Key::c('P')],
        name: "prune unused",
        help: "Delete every unused resource on this tab (asks first)",
        scope: Scope::Removable,
        footer: false,
    },
    // ---- misc ------------------------------------------------------------
    CommandSpec {
        command: Command::CopyId,
        keys: &[Key::c('y')],
        name: "copy id",
        help: "Copy the selected resource's id to the clipboard",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Filter,
        keys: &[Key::c('/')],
        name: "filter",
        help: "Fuzzy-filter the current list",
        scope: Scope::Global,
        footer: true,
    },
    CommandSpec {
        command: Command::ClearFilter,
        keys: &[Key::code(KeyCode::Esc)],
        name: "clear filter",
        help: "Drop the current filter and show everything again",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Palette,
        keys: &[Key::c(':')],
        name: "command palette",
        help: "Search every command by name",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::ToggleAll,
        keys: &[Key::c('a')],
        name: "toggle stopped",
        help: "Show or hide stopped containers",
        scope: Scope::Containers,
        footer: false,
    },
    CommandSpec {
        command: Command::SortNext,
        keys: &[Key::c('o')],
        name: "sort: next column",
        help: "Cycle the sort column",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::SortReverse,
        keys: &[Key::c('O')],
        name: "sort: reverse",
        help: "Reverse the sort direction",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Refresh,
        keys: &[Key::ctrl('r')],
        name: "refresh",
        help: "Re-read everything from the daemon now",
        scope: Scope::Global,
        footer: false,
    },
    CommandSpec {
        command: Command::Help,
        keys: &[Key::c('?')],
        name: "help",
        help: "Show every keybinding",
        scope: Scope::Global,
        footer: true,
    },
    CommandSpec {
        command: Command::Quit,
        keys: &[Key::c('q'), Key::ctrl('c')],
        name: "quit",
        help: "Leave dockyard",
        scope: Scope::Global,
        footer: true,
    },
];

/// Look up the command bound to a key press.
pub fn resolve(ev: &KeyEvent) -> Option<Command> {
    COMMANDS
        .iter()
        .find(|spec| spec.keys.iter().any(|k| k.matches(ev)))
        .map(|spec| spec.command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_at_least_one_key() {
        for spec in COMMANDS {
            assert!(!spec.keys.is_empty(), "{} has no key", spec.name);
        }
    }

    #[test]
    fn no_key_is_bound_twice() {
        let mut seen: Vec<(Key, &str)> = Vec::new();
        for spec in COMMANDS {
            for key in spec.keys {
                if let Some((_, other)) = seen.iter().find(|(k, _)| k == key) {
                    panic!(
                        "{} is bound to both {} and {}",
                        key.label(),
                        other,
                        spec.name
                    );
                }
                seen.push((*key, spec.name));
            }
        }
    }

    #[test]
    fn ctrl_is_discriminated() {
        let plain = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let with_ctrl = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(resolve(&plain), Some(Command::Restart));
        assert_eq!(resolve(&with_ctrl), Some(Command::Refresh));
    }

    #[test]
    fn shift_is_carried_by_the_character() {
        let lower = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        let upper = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(resolve(&lower), Some(Command::Exec));
        assert_eq!(resolve(&upper), Some(Command::Stop));
    }
}
