//! Colours and glyphs.
//!
//! Everything the UI draws pulls its colour from a named role here rather than
//! naming a colour directly, so the whole app can be re-skinned (or stripped
//! back to plain ANSI / monochrome) from one place.

use ratatui::style::{Color, Modifier, Style};

/// How adventurous we can be with non-ASCII glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Glyphs {
    /// Pure 7-bit ASCII. Always safe.
    Ascii,
    /// Geometric shapes and box drawing — present in essentially every
    /// monospace font. The default.
    #[default]
    Unicode,
    /// Nerd Font private-use icons. Only if the user opted in.
    Nerd,
}

/// Which colour space to render in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    /// A curated 24-bit palette (Tokyo Night). Consistent everywhere.
    #[default]
    TrueColor,
    /// The terminal's own 16 ANSI colours, so the app inherits the user's theme.
    Ansi,
    /// No colour at all — weight and reverse video only.
    Mono,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub palette: Palette,
    pub glyphs: Glyphs,

    // Structural roles.
    pub text: Color,
    pub subtle: Color,
    pub faint: Color,
    pub border: Color,
    pub border_focus: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub selection: Color,
    pub selection_text: Color,

    // Semantic roles.
    pub primary: Color,
    pub accent: Color,
    pub success: Color,
    pub warn: Color,
    pub danger: Color,
    pub info: Color,
}

impl Theme {
    pub fn new(palette: Palette, glyphs: Glyphs) -> Self {
        match palette {
            Palette::TrueColor => Self {
                palette,
                glyphs,
                text: Color::Rgb(0xC0, 0xCA, 0xF5),
                subtle: Color::Rgb(0x7A, 0x88, 0xB8),
                faint: Color::Rgb(0x56, 0x5F, 0x89),
                border: Color::Rgb(0x3B, 0x42, 0x61),
                border_focus: Color::Rgb(0x7A, 0xA2, 0xF7),
                surface: Color::Rgb(0x1A, 0x1B, 0x26),
                surface_alt: Color::Rgb(0x1F, 0x23, 0x35),
                selection: Color::Rgb(0x2E, 0x3C, 0x64),
                selection_text: Color::Rgb(0xE6, 0xEC, 0xFF),
                primary: Color::Rgb(0x7A, 0xA2, 0xF7),
                accent: Color::Rgb(0xBB, 0x9A, 0xF7),
                success: Color::Rgb(0x9E, 0xCE, 0x6A),
                warn: Color::Rgb(0xE0, 0xAF, 0x68),
                danger: Color::Rgb(0xF7, 0x76, 0x8E),
                info: Color::Rgb(0x7D, 0xCF, 0xFF),
            },
            Palette::Ansi => Self {
                palette,
                glyphs,
                text: Color::Reset,
                subtle: Color::Gray,
                faint: Color::DarkGray,
                border: Color::DarkGray,
                border_focus: Color::Blue,
                surface: Color::Reset,
                surface_alt: Color::Reset,
                selection: Color::Blue,
                selection_text: Color::Black,
                primary: Color::Blue,
                accent: Color::Magenta,
                success: Color::Green,
                warn: Color::Yellow,
                danger: Color::Red,
                info: Color::Cyan,
            },
            Palette::Mono => Self {
                palette,
                glyphs,
                text: Color::Reset,
                subtle: Color::Reset,
                faint: Color::Reset,
                border: Color::Reset,
                border_focus: Color::Reset,
                surface: Color::Reset,
                surface_alt: Color::Reset,
                selection: Color::Reset,
                selection_text: Color::Reset,
                primary: Color::Reset,
                accent: Color::Reset,
                success: Color::Reset,
                warn: Color::Reset,
                danger: Color::Reset,
                info: Color::Reset,
            },
        }
    }

    pub fn is_mono(&self) -> bool {
        self.palette == Palette::Mono
    }

    // ---- Convenience styles -------------------------------------------------

    pub fn base(&self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn dim(&self) -> Style {
        if self.is_mono() {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(self.subtle)
        }
    }

    pub fn faint_style(&self) -> Style {
        if self.is_mono() {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(self.faint)
        }
    }

    /// Border style for a pane, brighter when that pane has focus.
    pub fn border_style(&self, focused: bool) -> Style {
        if self.is_mono() {
            if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            }
        } else if focused {
            Style::default().fg(self.border_focus)
        } else {
            Style::default().fg(self.border)
        }
    }

    /// Title style for a pane, likewise focus-sensitive.
    pub fn title_style(&self, focused: bool) -> Style {
        if focused {
            Style::default()
                .fg(if self.is_mono() {
                    self.text
                } else {
                    self.border_focus
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.subtle)
        }
    }

    /// The highlighted table row.
    pub fn selected_style(&self, focused: bool) -> Style {
        if self.is_mono() {
            return Style::default().add_modifier(Modifier::REVERSED);
        }
        if focused {
            Style::default()
                .bg(self.selection)
                .fg(self.selection_text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(self.surface_alt)
        }
    }

    /// A dialog button. Only the focused one is filled, so the pair reads as a
    /// choice rather than as one recommendation next to some small print.
    /// `destructive` tints that fill red, which is what makes an armed "yes"
    /// on a delete look different from an armed "cancel".
    pub fn button_style(&self, focused: bool, destructive: bool) -> Style {
        if self.is_mono() {
            // No colour to spend, so the fill has to come from the modifier.
            return if focused {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default()
            };
        }
        if focused {
            Style::default()
                .fg(self.selection_text)
                .bg(if destructive {
                    self.danger
                } else {
                    self.primary
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.subtle).bg(self.surface_alt)
        }
    }

    /// Zebra striping for tables. Returns `None` for rows that should keep the
    /// terminal's own background.
    pub fn stripe(&self, index: usize) -> Option<Color> {
        if self.palette == Palette::TrueColor && index % 2 == 1 {
            Some(self.surface_alt)
        } else {
            None
        }
    }

    /// Style for a keycap in the footer / help.
    pub fn key_style(&self) -> Style {
        if self.is_mono() {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.warn).add_modifier(Modifier::BOLD)
        }
    }
}

/// Every glyph the UI uses, resolved once for the chosen [`Glyphs`] level.
#[derive(Debug, Clone, Copy)]
pub struct Symbols {
    pub running: &'static str,
    pub stopped: &'static str,
    pub paused: &'static str,
    pub restarting: &'static str,
    pub created: &'static str,
    pub dead: &'static str,
    pub unhealthy: &'static str,

    pub tab_containers: &'static str,
    pub tab_images: &'static str,
    pub tab_volumes: &'static str,
    pub tab_networks: &'static str,

    pub arrow_right: &'static str,
    /// Small triangles used as sort-direction indicators.
    pub arrow_down: &'static str,
    pub arrow_up: &'static str,
    /// Full-height arrows used when naming the cursor keys.
    pub key_up: &'static str,
    pub key_down: &'static str,
    pub bullet: &'static str,
    pub prompt: &'static str,
    pub check: &'static str,
    pub cross: &'static str,
    pub spinner: &'static [&'static str],
}

const SPINNER_BRAILLE: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_ASCII: &[&str] = &["|", "/", "-", "\\"];

impl Symbols {
    pub fn new(glyphs: Glyphs) -> Self {
        match glyphs {
            Glyphs::Ascii => Self {
                running: "*",
                stopped: "-",
                paused: "=",
                restarting: "~",
                created: "+",
                dead: "x",
                unhealthy: "!",
                tab_containers: "",
                tab_images: "",
                tab_volumes: "",
                tab_networks: "",
                arrow_right: ">",
                arrow_down: "v",
                arrow_up: "^",
                key_up: "up",
                key_down: "/down",
                bullet: "-",
                prompt: ">",
                check: "ok",
                cross: "x",
                spinner: SPINNER_ASCII,
            },
            Glyphs::Unicode => Self {
                running: "●",
                stopped: "○",
                paused: "◐",
                restarting: "◌",
                created: "◇",
                dead: "✖",
                unhealthy: "▲",
                tab_containers: "",
                tab_images: "",
                tab_volumes: "",
                tab_networks: "",
                arrow_right: "▸",
                arrow_down: "▾",
                arrow_up: "▴",
                key_up: "↑",
                key_down: "↓",
                bullet: "·",
                prompt: "❯",
                check: "✓",
                cross: "✗",
                spinner: SPINNER_BRAILLE,
            },
            Glyphs::Nerd => Self {
                running: "●",
                stopped: "○",
                paused: "◐",
                restarting: "◌",
                created: "◇",
                dead: "✖",
                unhealthy: "▲",
                // Classic Font Awesome codepoints, present in every Nerd Font.
                tab_containers: "\u{f1b2} ",
                tab_images: "\u{f487} ",
                tab_volumes: "\u{f1c0} ",
                tab_networks: "\u{f0e8} ",
                arrow_right: "▸",
                arrow_down: "▾",
                arrow_up: "▴",
                key_up: "↑",
                key_down: "↓",
                bullet: "·",
                prompt: "\u{f054} ",
                check: "\u{f00c}",
                cross: "\u{f00d}",
                spinner: SPINNER_BRAILLE,
            },
        }
    }

    pub fn spin(&self, frame: usize) -> &'static str {
        self.spinner[frame % self.spinner.len()]
    }
}

/// Box-drawing characters are far more widely supported than the geometric
/// shapes and arrows, but `--ascii` means *ascii*.
pub const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Rewrite the symbolic key names ([`crate::action::Key::label`] returns `⏎`,
/// `↑` and friends) into ASCII. Only used when the theme is in ASCII mode, so
/// the pretty forms stay the default.
pub fn asciify_key(label: &str) -> String {
    match label {
        "⏎" => "enter".into(),
        "⇧tab" => "s-tab".into(),
        "↑" => "up".into(),
        "↓" => "down".into(),
        "←" => "left".into(),
        "→" => "right".into(),
        other => other.to_string(),
    }
}
