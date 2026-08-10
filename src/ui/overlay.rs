//! Modal surfaces: help, command palette, confirmation, and toasts.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::action::{COMMANDS, Scope};
use crate::app::{App, ToastKind};
use crate::util::format;

/// Modal chrome: clear whatever is underneath, then draw a bordered box.
fn modal(frame: &mut Frame, app: &mut App, area: Rect, title: &str) -> Rect {
    app.hits.modal = Some(area);
    frame.render_widget(Clear, area);
    let block = crate::ui::bordered(app)
        .border_style(Style::default().fg(app.theme.border_focus))
        .style(Style::default().bg(app.theme.surface))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                title,
                Style::default()
                    .fg(app.theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

pub fn help(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<&crate::action::CommandSpec> = COMMANDS.iter().collect();
    let height = (rows.len() as u16 + 4).min(area.height.saturating_sub(2));
    let rect = crate::ui::centered(area, 74.min(area.width), height);
    let inner = modal(frame, app, rect, "keybindings");

    let lines: Vec<Line> = rows
        .iter()
        .map(|spec| {
            let keys = spec
                .keys
                .iter()
                .map(|k| crate::ui::key_label(app, k))
                .collect::<Vec<_>>()
                .join(" / ");
            Line::from(vec![
                Span::styled(format!("  {keys:<12}"), app.theme.key_style()),
                Span::styled(format!("{:<28}", spec.name), app.theme.base()),
                Span::styled(scope_label(spec.scope), app.theme.faint_style()),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);

    // The bottom line doubles as an about box — this is the only place the
    // daemon's platform details are worth the space.
    let hint = Line::from(vec![
        Span::styled("  any key to close", app.theme.faint_style()),
        Span::styled(
            format!("      {}", daemon_line(app)),
            app.theme.faint_style(),
        ),
    ]);
    let y = inner.y + inner.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(hint),
        Rect {
            y,
            height: 1,
            ..inner
        },
    );
}

fn daemon_line(app: &App) -> String {
    let d = &app.daemon;
    if d.version.is_empty() {
        return String::new();
    }
    let mut s = format!("docker {}", d.version);
    if !d.api_version.is_empty() {
        s.push_str(&format!(" · api {}", d.api_version));
    }
    if !d.os.is_empty() && !d.arch.is_empty() {
        s.push_str(&format!(" · {}/{}", d.os, d.arch));
    }
    s
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "",
        Scope::Containers => "containers",
        Scope::Removable => "any tab",
    }
}

// ---------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------

pub fn palette(frame: &mut Frame, app: &mut App, area: Rect) {
    let matches = app.palette_matches();
    let visible = matches.len().min(12);
    let height = (visible as u16 + 4).min(area.height.saturating_sub(2));
    let rect = crate::ui::centered(area, 80.min(area.width), height);
    let inner = modal(frame, app, rect, "commands");

    if inner.height == 0 {
        return;
    }

    // Prompt line.
    let prompt = Line::from(vec![
        Span::styled(
            format!(" {} ", app.symbols.prompt),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.palette_input.value.clone(), app.theme.base()),
        Span::styled("▏", Style::default().fg(app.theme.accent)),
    ]);
    frame.render_widget(Paragraph::new(prompt), Rect { height: 1, ..inner });

    let list_area = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    if list_area.height == 0 {
        return;
    }

    // Keep the cursor in view when the match list is longer than the box.
    let start = app
        .palette_cursor
        .saturating_sub(list_area.height as usize - 1);

    // Record where each visible entry landed, so it can be clicked. Rows are
    // laid out from `list_area.y` down, one per match from `start`.
    app.hits.palette_rows = matches
        .iter()
        .enumerate()
        .skip(start)
        .take(list_area.height as usize)
        .map(|(position, &index)| {
            let rect = Rect {
                y: list_area.y + (position - start) as u16,
                height: 1,
                ..list_area
            };
            (rect, index)
        })
        .collect();

    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(start)
        .take(list_area.height as usize)
        .map(|(position, &index)| {
            let spec = &COMMANDS[index];
            let selected = position == app.palette_cursor;
            let keys = spec
                .keys
                .first()
                .map(|k| crate::ui::key_label(app, k))
                .unwrap_or_default();

            let name_style = if selected {
                Style::default()
                    .fg(app.theme.selection_text)
                    .add_modifier(Modifier::BOLD)
            } else {
                app.theme.base()
            };
            let line = Line::from(vec![
                Span::styled(
                    if selected {
                        format!(" {} ", app.symbols.arrow_right)
                    } else {
                        "   ".into()
                    },
                    Style::default().fg(app.theme.accent),
                ),
                Span::styled(format!("{:<28}", spec.name), name_style),
                Span::styled(format!("{keys:<8}"), app.theme.key_style()),
                Span::styled(
                    format::truncate(spec.help, list_area.width.saturating_sub(42) as usize),
                    app.theme.faint_style(),
                ),
            ]);
            if selected {
                line.style(Style::default().bg(app.theme.selection))
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), list_area);
}

// ---------------------------------------------------------------------------
// Confirmation
// ---------------------------------------------------------------------------

pub fn confirm(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(job) = &app.confirm else { return };
    let (title, body) = job.confirm_prompt();

    let width = 60.min(area.width);
    let rect = crate::ui::centered(area, width, 8.min(area.height));
    app.hits.modal = Some(rect);
    frame.render_widget(Clear, rect);

    let block = crate::ui::bordered(app)
        .border_style(Style::default().fg(app.theme.danger))
        .style(Style::default().bg(app.theme.surface))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                title,
                Style::default()
                    .fg(app.theme.danger)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let text = Paragraph::new(vec![
        Line::raw(""),
        Line::from(Span::styled(body, app.theme.base())),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(text, inner);

    // Drawn as two discrete buttons rather than one centred sentence, so each
    // has a rectangle the mouse can be tested against.
    let y = inner.y + inner.height.saturating_sub(1);
    let yes_label = "  y  yes  ";
    let no_label = "  n / esc  cancel  ";
    let yes_w = format::width(yes_label) as u16;
    let no_w = format::width(no_label) as u16;
    let total = yes_w + no_w + 2;

    if total <= inner.width {
        let x = inner.x + (inner.width - total) / 2;
        let yes = Rect {
            x,
            y,
            width: yes_w,
            height: 1,
        };
        let no = Rect {
            x: x + yes_w + 2,
            y,
            width: no_w,
            height: 1,
        };

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                yes_label,
                Style::default()
                    .fg(app.theme.selection_text)
                    .bg(app.theme.danger)
                    .add_modifier(Modifier::BOLD),
            ))),
            yes,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                no_label,
                Style::default()
                    .fg(app.theme.text)
                    .bg(app.theme.surface_alt),
            ))),
            no,
        );

        app.hits.confirm_yes = Some(yes);
        app.hits.confirm_no = Some(no);
    }
}

// ---------------------------------------------------------------------------
// Toasts
// ---------------------------------------------------------------------------

pub fn toasts(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.toasts.is_empty() {
        return;
    }

    // Daemon errors are wordy; give them room rather than clipping the part
    // that says what actually went wrong.
    let max_width = area.width.saturating_sub(4).min(100);
    if max_width < 12 {
        return;
    }

    // Stack upwards from just above the footer, newest at the bottom.
    let count = app.toasts.len().min(4) as u16;
    let start = app.toasts.len().saturating_sub(count as usize);
    let bottom = area.y + area.height.saturating_sub(2);

    let theme = app.theme.clone();
    let symbols = app.symbols;
    let mut placed: Vec<(Rect, usize)> = Vec::new();

    for (row, toast) in app.toasts[start..].iter().enumerate() {
        let (icon, color) = match toast.kind {
            ToastKind::Success => (symbols.check, theme.success),
            ToastKind::Error => (symbols.cross, theme.danger),
            ToastKind::Info => (symbols.bullet, theme.info),
        };

        let text = format::truncate(&toast.text, max_width.saturating_sub(6) as usize);
        let width = (format::width(&text) as u16 + 6).min(max_width);
        let y = bottom.saturating_sub(count - 1 - row as u16);
        if y < area.y {
            continue;
        }
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width + 1),
            y,
            width,
            height: 1,
        };

        frame.render_widget(Clear, rect);
        let line = Line::from(vec![
            Span::styled(
                format!(" {icon} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(text, Style::default().fg(theme.text)),
            Span::raw(" "),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.surface_alt)),
            rect,
        );
        placed.push((rect, start + row));
    }

    app.hits.toasts = placed;
}
