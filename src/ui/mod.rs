pub mod detail;
pub mod footer;
pub mod header;
pub mod logs;
pub mod overlay;
pub mod tables;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::{App, Focus, Mode, Splitter, Tab};

/// Below this width the detail pane stacks under the list instead of beside it.
const NARROW: u16 = 96;
/// Below this width we stop drawing the detail pane at all — there's no room
/// for two panes and one of them would be unreadable.
const VERY_NARROW: u16 = 60;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Hit regions are rebuilt by this pass; anything left from the last frame
    // describes a layout that no longer exists.
    app.reset_hits();

    // A terminal this small can't show anything useful; say so rather than
    // panicking inside a layout constraint.
    if area.width < 30 || area.height < 8 {
        let msg = Paragraph::new("terminal too small").style(app.theme.dim());
        frame.render_widget(msg, area);
        return;
    }

    let banner_height = u16::from(app.daemon_error.is_some());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),             // header
            Constraint::Length(banner_height), // disconnect banner
            Constraint::Min(3),                // body
            Constraint::Length(1),             // footer
        ])
        .split(area);

    header::draw(frame, app, chunks[0]);
    if banner_height > 0 {
        draw_banner(frame, app, chunks[1]);
    }
    draw_body(frame, app, chunks[2]);
    footer::draw(frame, app, chunks[3]);

    // Overlays paint over everything below them.
    match app.mode {
        Mode::Help => overlay::help(frame, app, area),
        Mode::Palette => overlay::palette(frame, app, area),
        Mode::Confirm => overlay::confirm(frame, app, area),
        Mode::Filter | Mode::Normal => {}
    }

    overlay::toasts(frame, app, area);
}

fn draw_banner(frame: &mut Frame, app: &App, area: Rect) {
    let msg = app.daemon_error.as_deref().unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(
            format!(" {} docker unreachable ", app.symbols.unhealthy),
            Style::default()
                .fg(app.theme.danger)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            crate::util::format::truncate(msg, area.width.saturating_sub(24) as usize),
            app.theme.dim(),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let show_logs = app.tab == Tab::Containers && app.show_logs;
    let show_detail = app.show_detail && area.width >= VERY_NARROW;

    let (top, logs_area) = if show_logs {
        // The log pane gets a generous share but never squeezes the list below
        // a handful of rows. `log_height` honours a dragged size within that.
        let log_height = app.log_height(area.height);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(log_height)])
            .split(area);
        (rows[0], Some(rows[1]))
    } else {
        (area, None)
    };

    // Which way the detail pane splits decides which way its boundary drags.
    let detail_side = top.width >= NARROW;
    let (list_area, detail_area) = if show_detail {
        if detail_side {
            let width = app.detail_width(top.width);
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(40), Constraint::Length(width)])
                .split(top);
            (cols[0], Some(cols[1]))
        } else {
            let height = app.detail_height(top.height);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(height)])
                .split(top);
            (rows[0], Some(rows[1]))
        }
    } else {
        (top, None)
    };

    tables::draw(frame, app, list_area);

    if let Some(detail_area) = detail_area {
        app.hits.detail = Some(detail_area);
        app.hits.splitters.push((
            if detail_side {
                grab_column(detail_area)
            } else {
                grab_row(detail_area)
            },
            if detail_side {
                Splitter::DetailSide
            } else {
                Splitter::DetailBelow
            },
        ));
        detail::draw(frame, app, detail_area);
    }
    if let Some(logs_area) = logs_area {
        app.hits.logs = Some(logs_area);
        app.hits
            .splitters
            .push((grab_row(logs_area), Splitter::Logs));
        logs::draw(frame, app, logs_area);
    }
}

/// The grab region for a pane's top edge: that border row plus the one above
/// it, which belongs to the pane before. Two rows rather than one because a
/// single-row target is more of a dare than an affordance.
fn grab_row(pane: Rect) -> Rect {
    Rect {
        x: pane.x,
        y: pane.y.saturating_sub(1),
        width: pane.width,
        height: 2,
    }
}

/// The same for a pane's left edge.
fn grab_column(pane: Rect) -> Rect {
    Rect {
        x: pane.x.saturating_sub(1),
        y: pane.y,
        width: 2,
        height: pane.height,
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A pane block with the house style: rounded borders, focus-sensitive colour,
/// and a title that reads as a breadcrumb.
pub fn pane<'a>(app: &App, title: Vec<Span<'a>>, focused: bool) -> Block<'a> {
    bordered(app)
        .border_style(app.theme.border_style(focused))
        .title(Line::from(title))
}

/// A bordered block in whichever line style the theme allows.
pub fn bordered<'a>(app: &App) -> Block<'a> {
    let block = Block::bordered();
    if app.theme.glyphs == theme::Glyphs::Ascii {
        block.border_set(theme::ASCII_BORDER)
    } else {
        block.border_type(BorderType::Rounded)
    }
}

/// A key's display name, honouring the theme's glyph level.
pub fn key_label(app: &App, key: &crate::action::Key) -> String {
    let label = key.label();
    if app.theme.glyphs == theme::Glyphs::Ascii {
        theme::asciify_key(&label)
    } else {
        label
    }
}

/// Standard pane title: `─ name ── detail ─`.
pub fn title<'a>(app: &App, name: &'a str, detail: Option<String>, focused: bool) -> Vec<Span<'a>> {
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(name, app.theme.title_style(focused)),
    ];
    if let Some(detail) = detail {
        spans.push(Span::styled(format!("  {detail}"), app.theme.dim()));
    }
    spans.push(Span::raw(" "));
    spans
}

/// Centre a box of the given size inside `area`, clamped to fit.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Resolve the widths a set of table constraints will produce, so cell text can
/// be truncated to exactly the space it will get.
pub fn column_widths(width: u16, constraints: &[Constraint], spacing: u16) -> Vec<u16> {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height: 1,
    };
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints.to_vec())
        .spacing(spacing)
        .split(area)
        .iter()
        .map(|r| r.width)
        .collect()
}

/// Focus-aware helper used by every pane.
pub fn is_focused(app: &App, focus: Focus) -> bool {
    app.focus == focus && matches!(app.mode, Mode::Normal | Mode::Filter)
}

/// Render `text` with the characters that the active filter matched picked out.
/// Falls back to a single plain span when there's no filter, or when `text` no
/// longer matches (which happens once it's been truncated).
pub fn highlight(
    text: String,
    needle: &str,
    base: ratatui::style::Style,
    hit: ratatui::style::Style,
) -> Vec<Span<'static>> {
    if needle.is_empty() {
        return vec![Span::styled(text, base)];
    }
    let Some(m) = crate::util::fuzzy::match_str(&text, needle) else {
        return vec![Span::styled(text, base)];
    };

    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut buffer_is_hit = false;

    for (i, c) in text.chars().enumerate() {
        let is_hit = m.indices.binary_search(&i).is_ok();
        if is_hit != buffer_is_hit && !buffer.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut buffer),
                if buffer_is_hit { hit } else { base },
            ));
        }
        buffer_is_hit = is_hit;
        buffer.push(c);
    }
    if !buffer.is_empty() {
        spans.push(Span::styled(buffer, if buffer_is_hit { hit } else { base }));
    }
    spans
}
