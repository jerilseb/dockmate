//! The title bar: the tab strip, and a live summary of the daemon.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Tab};
use crate::util::format;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let sym = app.symbols;

    let summary = summary(app);
    let block = crate::ui::bordered(app)
        .border_style(Style::default().fg(theme.border))
        .title(
            Line::from(vec![Span::styled(format!(" {summary} "), theme.dim())])
                .alignment(Alignment::Right),
        );

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Tab strip. Built by hand rather than with `Tabs` so the icon, label and
    // count can each carry their own style — and so each tab's extent is known
    // and can be recorded for the mouse.
    let mut spans: Vec<Span> = Vec::new();
    let mut x = inner.x;
    for tab in Tab::ALL {
        let active = tab == app.tab;
        let icon = match tab {
            Tab::Containers => sym.tab_containers,
            Tab::Images => sym.tab_images,
            Tab::Volumes => sym.tab_volumes,
            Tab::Networks => sym.tab_networks,
        };
        let count = match tab {
            Tab::Containers => app.containers.len(),
            Tab::Images => app.images.len(),
            Tab::Volumes => app.volumes.len(),
            Tab::Networks => app.networks.len(),
        };

        let label_style = if active {
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.subtle)
        };

        let marker = if active { sym.arrow_right } else { " " };
        let count_text = format!(" {count}");

        spans.push(Span::raw(" "));
        spans.push(Span::styled(marker, Style::default().fg(theme.primary)));
        spans.push(Span::raw(" "));
        if !icon.is_empty() {
            spans.push(Span::styled(icon, label_style));
        }
        spans.push(Span::styled(tab.title(), label_style));
        spans.push(Span::styled(
            count_text.clone(),
            if active {
                Style::default().fg(theme.accent)
            } else {
                theme.faint_style()
            },
        ));
        spans.push(Span::raw("  "));

        // Everything from the leading space through the trailing gap is a click
        // target, so there are no dead pixels between tabs.
        let width =
            (3 + format::width(icon) + format::width(tab.title()) + format::width(&count_text) + 2)
                as u16;
        if x < inner.right() {
            app.hits.tabs.push((
                Rect {
                    x,
                    y: inner.y,
                    width: width.min(inner.right() - x),
                    height: inner.height,
                },
                tab,
            ));
        }
        x = x.saturating_add(width);
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// The right-hand status text: daemon version plus how many containers are up.
fn summary(app: &App) -> String {
    let running = app.running_count();
    let total = app.containers.len();

    let version = if app.daemon.version.is_empty() {
        "connecting…".to_string()
    } else {
        format!("docker {}", app.daemon.version)
    };

    if total == 0 {
        version
    } else {
        format!(
            "{version}  {}  {running}/{total} running",
            app.symbols.bullet
        )
    }
}
