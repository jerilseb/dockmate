//! The log pane.
//!
//! Lines are laid out from the bottom up so the newest output is always where
//! the eye expects it, and so wrapped lines consume the right number of rows
//! when computing how far back to start.

use ansi_to_tui::IntoText;
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use crate::app::{App, Focus};
use crate::docker::logs::{LogLine, Stream};
use crate::util::format;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let focused = crate::ui::is_focused(app, Focus::Logs);
    let name = app
        .selected_container()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "-".into());

    let block = crate::ui::pane(
        app,
        crate::ui::title(app, "logs", Some(status(app, &name)), focused),
        focused,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if let Some(err) = &app.logs.error {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", app.symbols.cross),
                Style::default().fg(app.theme.danger),
            ),
            Span::styled(err.clone(), app.theme.dim()),
        ]));
        frame.render_widget(msg, inner);
        return;
    }

    if app.logs.lines.is_empty() {
        let text = if app.logs.loading {
            format!("{} attaching…", app.symbols.spin(app.spinner / 2))
        } else if app.logs.container_id.is_none() {
            "no container selected".to_string()
        } else {
            "no output yet".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, app.theme.dim()))),
            inner,
        );
        return;
    }

    // `scroll` counts lines back from the newest; find the slice that fills the
    // pane ending there.
    let total = app.logs.lines.len();
    let end = total.saturating_sub(app.logs.scroll);
    let width = inner.width as usize;

    let mut rendered: Vec<Line> = Vec::new();
    let mut rows = 0usize;
    for raw in app.logs.lines.iter().take(end).rev() {
        let line = render_line(app, raw, width);
        // A wrapped line eats more than one row, so account for it or the pane
        // scrolls by the wrong amount.
        let cost = if app.logs.wrap {
            let w = line.width().max(1);
            w.div_ceil(width.max(1))
        } else {
            1
        };
        if rows + cost > inner.height as usize && !rendered.is_empty() {
            break;
        }
        rows += cost;
        rendered.push(line);
    }
    rendered.reverse();

    let mut para = Paragraph::new(rendered);
    if app.logs.wrap {
        para = para.wrap(Wrap { trim: false });
    }
    frame.render_widget(para, inner);

    // A scrollbar only earns its column once there's more than a screenful.
    // It rides the right border, so inset it vertically to leave the corners
    // alone, and drop the track so only the thumb paints over the border.
    if total > inner.height as usize {
        let mut state = ScrollbarState::new(total.saturating_sub(inner.height as usize))
            .position(end.saturating_sub(inner.height as usize));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(app.theme.border_focus))
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

/// The breadcrumb after `logs`: which container, and whether we're following.
fn status(app: &App, name: &str) -> String {
    let mut parts = vec![name.to_string()];
    if app.logs.follow {
        parts.push("following".into());
    } else {
        parts.push(format!("paused {} lines back", app.logs.scroll));
    }
    if app.logs.wrap {
        parts.push("wrap".into());
    }
    parts.join(&format!("  {}  ", app.symbols.bullet))
}

fn render_line<'a>(app: &App, line: &'a LogLine, width: usize) -> Line<'a> {
    let mut spans: Vec<Span> = Vec::new();

    if app.logs.timestamps
        && let Some(ts) = &line.timestamp
    {
        spans.push(Span::styled(format!("{ts} "), app.theme.faint_style()));
    }

    // stderr gets a subtle tint so failures stand out in a mixed stream.
    let base = if line.stream == Stream::Stderr {
        Style::default().fg(app.theme.danger)
    } else {
        app.theme.base()
    };

    let budget = width.saturating_sub(if app.logs.timestamps { 9 } else { 0 });
    let text = if app.logs.wrap {
        line.text.clone()
    } else {
        format::truncate(&line.text, budget)
    };

    // Container output frequently carries its own ANSI colouring; keep it when
    // we can parse it, and fall back to the plain text when we can't.
    match text.as_bytes().into_text() {
        Ok(parsed) if parsed.lines.len() == 1 && text.contains('\x1b') => {
            let mut parsed_line = parsed.lines.into_iter().next().unwrap();
            spans.append(&mut parsed_line.spans);
        }
        _ => spans.push(Span::styled(text, base)),
    }

    Line::from(spans)
}
