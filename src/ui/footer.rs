//! The contextual key bar, and the filter prompt that replaces it while typing.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::{COMMANDS, Command, Scope};
use crate::app::{App, Mode, Tab};
use crate::util::format;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    if app.mode == Mode::Filter {
        draw_filter(frame, app, area);
        return;
    }
    draw_keys(frame, app, area);
}

fn draw_filter(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let input = &app.filter[app.tab.index()];
    let matched = app.view().len();

    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.symbols.prompt),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(input.value.clone(), Style::default().fg(theme.text)),
        Span::styled("▏", Style::default().fg(theme.accent)),
        Span::styled(
            format!("   {matched} match{}", if matched == 1 { "" } else { "es" }),
            theme.faint_style(),
        ),
        Span::styled("   ⏎ keep   esc clear", theme.faint_style()),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

fn draw_keys(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let mut used = 1usize;

    // Movement is always first — it's the one hint that's never contextual.
    let move_keys = format!("{}{}", app.symbols.key_up, app.symbols.key_down);
    for (keys, label) in [(move_keys.as_str(), "move"), ("tab", "switch")] {
        push_hint(&mut spans, &mut used, app, keys, label);
    }

    for spec in COMMANDS.iter().filter(|s| s.footer) {
        if !applies(spec.scope, app.tab) {
            continue;
        }
        // `start` and `stop` are mutually exclusive in practice; show the one
        // that would actually do something to the selected container.
        if let Some(c) = app.selected_container() {
            if spec.command == Command::Start && c.state.is_live() {
                continue;
            }
            if spec.command == Command::Stop && !c.state.is_live() {
                continue;
            }
        }

        let keys: String = spec
            .keys
            .iter()
            .take(1)
            .map(|k| crate::ui::key_label(app, k))
            .collect::<Vec<_>>()
            .join("");
        let label = short_label(spec.command, app);
        if used + keys.len() + label.len() + 3 > area.width as usize {
            break;
        }
        push_hint(&mut spans, &mut used, app, &keys, label);
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn push_hint(spans: &mut Vec<Span<'static>>, used: &mut usize, app: &App, key: &str, label: &str) {
    spans.push(Span::styled(key.to_string(), app.theme.key_style()));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(label.to_string(), app.theme.dim()));
    spans.push(Span::raw("   "));
    *used += format::width(key) + format::width(label) + 4;
}

/// Footer labels are terser than the help text, and a couple of them change
/// with state so the bar always describes what the key will actually do.
fn short_label(command: Command, app: &App) -> &'static str {
    match command {
        Command::ToggleDetail => {
            if app.show_detail {
                "hide details"
            } else {
                "details"
            }
        }
        Command::ToggleLogs => {
            if app.show_logs {
                "hide logs"
            } else {
                "logs"
            }
        }
        Command::Start => "start",
        Command::Stop => "stop",
        Command::Restart => "restart",
        Command::Exec => "shell",
        Command::Remove => "delete",
        Command::Filter => "filter",
        Command::Help => "help",
        Command::Quit => "quit",
        _ => "",
    }
}

fn applies(scope: Scope, tab: Tab) -> bool {
    match scope {
        Scope::Global => true,
        Scope::Containers => tab == Tab::Containers,
        Scope::Removable => true,
    }
}
