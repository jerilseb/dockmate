//! The resource tables.
//!
//! Column widths are resolved up front with the same constraints the `Table`
//! widget uses, so each cell can be truncated to exactly the space it will get
//! rather than relying on the widget to clip mid-glyph.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, HighlightSpacing, Paragraph, Row, Table, TableState};

use crate::app::{App, Focus, Group, Sort, Tab, ViewRow};
use crate::docker::model::{Health, State};
use crate::ui::{self, theme::Theme};
use crate::util::format;

const SPACING: u16 = 1;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = ui::is_focused(app, Focus::List);
    let title = ui::title(app, tab_label(app.tab), Some(subtitle(app)), focused);
    let sort = sort_label(app);

    // Both titles share the one border row, so the corner only gets to claim
    // its space once the pane's own name has had its. Two cells for the corners
    // themselves, two for the padding `corner` adds.
    let room = ui::spans_width(&title) + format::width(&sort) + 4 <= area.width as usize;

    let mut block = ui::pane(app, title, focused);
    if room {
        block = block.title_top(ui::corner(app, sort));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.view().is_empty() {
        empty_state(frame, app, inner);
        return;
    }

    match app.tab {
        Tab::Containers => containers(frame, app, inner, focused),
        Tab::Images => images(frame, app, inner, focused),
        Tab::Volumes => volumes(frame, app, inner, focused),
        Tab::Networks => networks(frame, app, inner, focused),
    }
}

fn tab_label(tab: Tab) -> &'static str {
    match tab {
        Tab::Containers => "containers",
        Tab::Images => "images",
        Tab::Volumes => "volumes",
        Tab::Networks => "networks",
    }
}

/// `12/31 · 3 stacks · filtered` — the state of the list at a glance.
///
/// The sort lives in the opposite corner rather than here: it describes the
/// columns, so it belongs over the end of them, and a subtitle that grows a
/// stack count and a filter marker is the first thing a narrow pane truncates.
fn subtitle(app: &App) -> String {
    let shown = app.visible_items();
    let total = app.total_rows();
    let mut s = if shown == total {
        format!("{total}")
    } else {
        format!("{shown}/{total}")
    };
    if app.grouping() {
        s.push_str(&format!(
            "  {}  {} stack{}",
            app.symbols.bullet,
            app.groups.len(),
            if app.groups.len() == 1 { "" } else { "s" }
        ));
    }
    if !app.filter[app.tab.index()].is_empty() {
        s.push_str(&format!("  {}  filtered", app.symbols.bullet));
    }
    s
}

/// `cpu ▾` — the column the list is sorted by, and which way it runs. Drawn in
/// the pane's right-hand corner, over the columns it's talking about.
///
/// Takes its arrow from the same helper as the column chevron, so the two can't
/// end up pointing opposite ways on the same screen.
fn sort_label(app: &App) -> String {
    let arrow = if app.sort_descending() {
        app.symbols.arrow_down
    } else {
        app.symbols.arrow_up
    };
    format!("{} {arrow}", app.sort().label())
}

fn empty_state(frame: &mut Frame, app: &App, area: Rect) {
    let filtered = !app.filter[app.tab.index()].is_empty();
    let text = if filtered {
        "nothing matches this filter"
    } else {
        match app.tab {
            Tab::Containers if !app.show_stopped => {
                "no running containers  (press a to show stopped)"
            }
            Tab::Containers => "no containers",
            Tab::Images => "no images",
            Tab::Volumes => "no volumes",
            Tab::Networks => "no networks",
        }
    };
    let para = Paragraph::new(Line::from(Span::styled(text, app.theme.dim())))
        .alignment(Alignment::Center);
    // Sit the message a third of the way down rather than glued to the top.
    let y = area.y + area.height / 3;
    frame.render_widget(
        para,
        Rect {
            y,
            height: 1,
            ..area
        },
    );
}

/// The primary identifier column, with the characters an active filter matched
/// picked out so it's obvious *why* a row survived the filter.
fn name_cell(theme: &Theme, text: String, needle: &str, muted: bool) -> Cell<'static> {
    let base = if muted {
        Style::default().fg(theme.subtle)
    } else {
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
    };
    let hit = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    Cell::from(Line::from(ui::highlight(text, needle, base, hit)))
}

/// A table column: what it's called, how it aligns, and which sort clicking its
/// header selects (`None` for columns you can't sort by).
struct Col {
    title: &'static str,
    align: Alignment,
    sort: Option<Sort>,
}

const fn col(title: &'static str, align: Alignment, sort: Option<Sort>) -> Col {
    Col { title, align, sort }
}

/// Shared table plumbing: header row styling, selection, striping, and
/// recording where everything landed so the mouse can hit it.
fn render<'a>(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    focused: bool,
    cols: &[Col],
    constraints: &[Constraint],
    rows: Vec<Row<'a>>,
) {
    let theme = app.theme.clone();
    let active_sort = app.sort();
    // Only the active column is marked, so exactly one header grows by the two
    // cells the chevron costs — every fixed-width column has that much slack.
    let chevron = if app.sort_descending() {
        app.symbols.arrow_down
    } else {
        app.symbols.arrow_up
    };

    let header_cells: Vec<Cell> = cols
        .iter()
        .map(|c| {
            // The sorted column is picked out so the header doubles as an
            // indicator of what you're looking at, and carries a chevron for
            // which way it runs.
            let active = c.sort == Some(active_sort);
            let style = if active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.faint)
                    .add_modifier(Modifier::BOLD)
            };
            let mut spans = vec![Span::styled(c.title, style)];
            if active {
                spans.push(Span::styled(format!(" {chevron}"), style));
            }
            Cell::from(Line::from(spans).alignment(c.align))
        })
        .collect();

    let table = Table::new(rows, constraints.to_vec())
        .header(Row::new(header_cells).height(1).bottom_margin(0))
        .column_spacing(SPACING)
        .row_highlight_style(theme.selected_style(focused))
        .highlight_spacing(HighlightSpacing::Never);

    let mut state = TableState::default()
        .with_selected(app.cursor())
        .with_offset(app.offset[app.tab.index()]);

    frame.render_stateful_widget(table, area, &mut state);
    // Remember where the viewport ended up so it doesn't jump on the next draw.
    app.offset[app.tab.index()] = state.offset();

    // Record hit regions. The header occupies the first row; the data rows
    // follow, one per line, starting at the viewport offset.
    let widths = ui::column_widths(area.width, constraints, SPACING);
    let mut x = area.x;
    for (c, w) in cols.iter().zip(&widths) {
        if let Some(sort) = c.sort
            && x < area.right()
        {
            app.hits.list_headers.push((
                Rect {
                    x,
                    y: area.y,
                    width: (*w).min(area.right() - x),
                    height: 1,
                },
                sort,
            ));
        }
        x = x.saturating_add(w + SPACING);
    }
    app.hits.list_body = Some(Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    });
    app.hits.list_offset = state.offset();
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

fn containers(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    // Columns drop out in reverse priority order as the pane narrows. Without
    // this the fixed-width columns keep their space and squeeze the name — the
    // one column you can't identify a row without — down to nothing.
    let w = area.width;
    let show_load = w >= 42;
    let show_image = w >= 70;
    let show_ports = w >= 100;

    // `Fill` weights rather than percentages: the text columns share whatever
    // is left after the fixed ones, in a ratio that keeps names readable
    // without letting them swallow the row.
    let mut constraints = vec![Constraint::Length(2), Constraint::Fill(3)];
    let mut cols: Vec<Col> = vec![
        col("", Alignment::Left, None),
        col("NAME", Alignment::Left, Some(Sort::Name)),
    ];

    let mut i_image = None;
    if show_image {
        constraints.push(Constraint::Fill(4));
        cols.push(col("IMAGE", Alignment::Left, None));
        i_image = Some(constraints.len() - 1);
    }

    constraints.push(Constraint::Length(12));
    cols.push(col("STATE", Alignment::Left, Some(Sort::State)));
    let i_state = constraints.len() - 1;

    let mut i_load = None;
    if show_load {
        constraints.push(Constraint::Length(7));
        cols.push(col("CPU", Alignment::Right, Some(Sort::Cpu)));
        constraints.push(Constraint::Length(8));
        cols.push(col("MEM", Alignment::Right, Some(Sort::Mem)));
        i_load = Some(constraints.len() - 2);
    }

    let mut i_ports = None;
    if show_ports {
        constraints.push(Constraint::Fill(4));
        cols.push(col("PORTS", Alignment::Left, None));
        i_ports = Some(constraints.len() - 1);
    }

    let widths = ui::column_widths(area.width, &constraints, SPACING);
    let theme = app.theme.clone();
    let needle = app.filter[app.tab.index()].value.clone();
    let sym = app.symbols;
    let spinner = app.spinner;

    let grouped = app.grouping();
    // Zebra striping counts rows within a stack, not down the whole list, so each
    // group starts on the same footing and the headers stay legible.
    let mut stripe_at = 0usize;

    let rows: Vec<Row> = app
        .view()
        .iter()
        .filter_map(|row| {
            let index = match row {
                ViewRow::Group(g) => {
                    stripe_at = 0;
                    let group = app.groups.get(*g)?;
                    return Some(group_row(&theme, &sym, group, &needle, widths[1], i_state));
                }
                ViewRow::Item(index) => *index,
            };
            let position = stripe_at;
            stripe_at += 1;
            let c = app.containers.get(index)?;
            let stat = app.latest_stat(&c.id);
            let busy = app.pending.get(&c.id);

            let (dot, dot_style) = status_dot(&theme, &sym, c.state, c.health);
            let marker = match busy {
                Some(_) => Span::styled(sym.spin(spinner / 2), Style::default().fg(theme.info)),
                None => Span::styled(dot, dot_style),
            };
            // A column of glyphs jammed against the border reads badly.
            let marker = vec![Span::raw(" "), marker];

            let state_text = match busy {
                Some(verb) => format::truncate(verb, widths[i_state] as usize),
                None => {
                    format::truncate(&state_label(c.state, &c.status), widths[i_state] as usize)
                }
            };

            // Under a header the stack name is already on screen, so the row
            // says which *service* it is — `postgres`, not
            // `argilla-postgres-1`. Indented, so the two levels read apart.
            let name = if grouped {
                format!(
                    "  {}",
                    format::truncate(c.service_label(), widths[1].saturating_sub(2) as usize)
                )
            } else {
                format::truncate(&c.name, widths[1] as usize)
            };

            let mut cells = vec![
                Cell::from(Line::from(marker)),
                name_cell(&theme, name, &needle, false),
            ];

            if let Some(i) = i_image {
                cells.push(Cell::from(Span::styled(
                    // The interesting half of a registry-qualified image name
                    // is the end, so trim from the left.
                    format::truncate_start(&c.image, widths[i] as usize),
                    theme.dim(),
                )));
            }

            cells.push(Cell::from(Span::styled(
                state_text,
                if busy.is_some() {
                    Style::default().fg(theme.info)
                } else {
                    Style::default().fg(state_color(&theme, c.state))
                },
            )));

            if i_load.is_some() {
                cells.push(Cell::from(
                    Line::from(Span::styled(
                        stat.map(|s| format!("{:.1}%", s.cpu_percent))
                            .unwrap_or_else(|| "-".into()),
                        load_style(&theme, stat.map(|s| s.cpu_percent)),
                    ))
                    .alignment(Alignment::Right),
                ));
                cells.push(Cell::from(
                    Line::from(Span::styled(
                        stat.map(|s| format::bytes(s.mem_bytes))
                            .unwrap_or_else(|| "-".into()),
                        load_style(&theme, stat.and_then(|s| s.mem_percent())),
                    ))
                    .alignment(Alignment::Right),
                ));
            }

            if let Some(i) = i_ports {
                cells.push(Cell::from(Span::styled(
                    format::truncate(&c.ports_display(), widths[i] as usize),
                    theme.faint_style(),
                )));
            }

            let mut row = Row::new(cells).height(1);
            if let Some(bg) = theme.stripe(position) {
                row = row.style(Style::default().bg(bg));
            }
            Some(row)
        })
        .collect();

    render(frame, app, area, focused, &cols, &constraints, rows);
}

/// A stack's header row: a disclosure triangle, the stack name, and how much of
/// it is up. The count sits in the STATE column so it lines up with the states of
/// the rows underneath.
fn group_row<'a>(
    theme: &Theme,
    sym: &crate::ui::theme::Symbols,
    group: &Group,
    needle: &str,
    name_width: u16,
    state_index: usize,
) -> Row<'a> {
    let chevron = if group.collapsed {
        sym.arrow_right
    } else {
        sym.arrow_down
    };
    // The standalone bucket isn't a deployment, so it doesn't get the colour that
    // says "this is a thing someone shipped".
    let label_style = if group.is_standalone() {
        Style::default()
            .fg(theme.subtle)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD)
    };

    let mut cells = vec![
        Cell::from(Line::from(vec![
            Span::raw(" "),
            Span::styled(chevron, Style::default().fg(theme.faint)),
        ])),
        Cell::from(Line::from(ui::highlight(
            format::truncate(group.label(), name_width as usize),
            needle,
            label_style,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))),
    ];
    while cells.len() < state_index {
        cells.push(Cell::from(""));
    }
    cells.push(Cell::from(Span::styled(
        if group.running == group.items {
            format!("{} up", group.items)
        } else {
            format!("{}/{} up", group.running, group.items)
        },
        theme.dim(),
    )));

    Row::new(cells).height(1)
}

/// Prefer Docker's own status text over the bare state — `Up 2 days` and
/// `Exited (137)` both say more than `running` and `exited` do.
fn state_label(state: State, status: &str) -> String {
    if status.is_empty() {
        return state.label().to_string();
    }
    // For a running container the parenthetical is the health, which the dot
    // already shows: `Up 2 days (healthy)` → `Up 2 days`.
    if status.starts_with("Up") {
        return status.split(" (").next().unwrap_or(status).to_string();
    }
    // For a stopped one it's the exit code, which is the whole story:
    // `Exited (137) 2 days ago` → `Exited (137)`.
    match status.split_once(')') {
        Some((head, _)) => format!("{head})"),
        None => status.to_string(),
    }
}

fn state_color(theme: &Theme, state: State) -> ratatui::style::Color {
    match state {
        State::Running => theme.success,
        State::Restarting => theme.accent,
        State::Paused => theme.warn,
        State::Created => theme.info,
        State::Stopping | State::Removing => theme.warn,
        State::Exited => theme.subtle,
        State::Dead | State::Unknown => theme.danger,
    }
}

fn status_dot(
    theme: &Theme,
    sym: &crate::ui::theme::Symbols,
    state: State,
    health: Health,
) -> (&'static str, Style) {
    // Health, when present, is more informative than the coarse state.
    if state.is_running() {
        return match health {
            Health::Unhealthy => (sym.unhealthy, Style::default().fg(theme.danger)),
            Health::Starting => (sym.restarting, Style::default().fg(theme.warn)),
            _ => (sym.running, Style::default().fg(theme.success)),
        };
    }
    match state {
        State::Paused => (sym.paused, Style::default().fg(theme.warn)),
        State::Created => (sym.created, Style::default().fg(theme.info)),
        State::Dead => (sym.dead, Style::default().fg(theme.danger)),
        _ => (sym.stopped, Style::default().fg(theme.faint)),
    }
}

/// Colour load figures by how alarming they are, so a hot container stands out
/// without having to read the number.
fn load_style(theme: &Theme, percent: Option<f64>) -> Style {
    match percent {
        None => theme.faint_style(),
        Some(p) if p >= 80.0 => Style::default().fg(theme.danger),
        Some(p) if p >= 50.0 => Style::default().fg(theme.warn),
        Some(p) if p >= 1.0 => Style::default().fg(theme.text),
        Some(_) => theme.dim(),
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

fn images(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let constraints = vec![
        Constraint::Length(1),
        Constraint::Min(20),    // repository
        Constraint::Length(16), // tag
        Constraint::Length(12), // id
        Constraint::Length(9),  // size
        Constraint::Length(9),  // age
        Constraint::Length(5),  // in use
    ];
    let cols = vec![
        col("", Alignment::Left, None),
        col("REPOSITORY", Alignment::Left, Some(Sort::Name)),
        col("TAG", Alignment::Left, None),
        col("ID", Alignment::Left, None),
        col("SIZE", Alignment::Right, Some(Sort::Size)),
        col("AGE", Alignment::Right, Some(Sort::Created)),
        col("USED", Alignment::Right, None),
    ];

    let widths = ui::column_widths(area.width, &constraints, SPACING);
    let theme = app.theme.clone();
    let needle = app.filter[app.tab.index()].value.clone();
    let sym = app.symbols;

    let rows: Vec<Row> = app
        .view()
        .iter()
        .enumerate()
        .filter_map(|(position, row)| {
            // These tabs are never grouped, so every row is a resource.
            let ViewRow::Item(index) = *row else {
                return None;
            };
            let img = app.images.get(index)?;
            let in_use = img.containers > 0;

            let (dot, dot_style) = if img.dangling {
                (sym.dead, Style::default().fg(theme.warn))
            } else if in_use {
                (sym.running, Style::default().fg(theme.success))
            } else {
                (sym.stopped, Style::default().fg(theme.faint))
            };

            let cells = vec![
                Cell::from(Span::styled(dot, dot_style)),
                name_cell(
                    &theme,
                    format::truncate_start(&img.repository, widths[1] as usize),
                    &needle,
                    false,
                ),
                Cell::from(Span::styled(
                    format::truncate(&img.tag, widths[2] as usize),
                    Style::default().fg(if img.dangling { theme.warn } else { theme.info }),
                )),
                Cell::from(Span::styled(format::short_id(&img.id), theme.faint_style())),
                Cell::from(
                    Line::from(Span::styled(format::bytes_i64(img.size), theme.base()))
                        .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(Span::styled(
                        format::duration_short(chrono::Utc::now().timestamp() - img.created),
                        theme.dim(),
                    ))
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(Span::styled(
                        if in_use {
                            img.containers.to_string()
                        } else {
                            "-".into()
                        },
                        if in_use {
                            theme.base()
                        } else {
                            theme.faint_style()
                        },
                    ))
                    .alignment(Alignment::Right),
                ),
            ];

            let mut row = Row::new(cells).height(1);
            if let Some(bg) = theme.stripe(position) {
                row = row.style(Style::default().bg(bg));
            }
            Some(row)
        })
        .collect();

    render(frame, app, area, focused, &cols, &constraints, rows);
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

fn volumes(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let wide = area.width >= 90;
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(20),   // name
        Constraint::Length(9), // driver
        Constraint::Length(5), // used by
        Constraint::Length(8), // size
        Constraint::Length(9), // age
    ];
    let mut cols = vec![
        col("", Alignment::Left, None),
        col("NAME", Alignment::Left, Some(Sort::Name)),
        col("DRIVER", Alignment::Left, None),
        col("USED", Alignment::Right, None),
        col("SIZE", Alignment::Right, Some(Sort::Size)),
        col("AGE", Alignment::Right, Some(Sort::Created)),
    ];
    if wide {
        constraints.push(Constraint::Percentage(30));
        cols.push(col("MOUNTPOINT", Alignment::Left, None));
    }

    let widths = ui::column_widths(area.width, &constraints, SPACING);
    let theme = app.theme.clone();
    let needle = app.filter[app.tab.index()].value.clone();
    let sym = app.symbols;
    let measuring = app.measuring_volumes;
    let spinner = app.spinner;

    let rows: Vec<Row> = app
        .view()
        .iter()
        .enumerate()
        .filter_map(|(position, row)| {
            // These tabs are never grouped, so every row is a resource.
            let ViewRow::Item(index) = *row else {
                return None;
            };
            let v = app.volumes.get(index)?;
            let (dot, dot_style) = if v.used_by > 0 {
                (sym.running, Style::default().fg(theme.success))
            } else {
                (sym.stopped, Style::default().fg(theme.faint))
            };

            let mut cells = vec![
                Cell::from(Span::styled(dot, dot_style)),
                name_cell(
                    &theme,
                    format::truncate(&v.name, widths[1] as usize),
                    &needle,
                    false,
                ),
                Cell::from(Span::styled(
                    format::truncate(&v.driver, widths[2] as usize),
                    theme.dim(),
                )),
                Cell::from(
                    Line::from(Span::styled(
                        if v.used_by > 0 {
                            v.used_by.to_string()
                        } else {
                            "-".into()
                        },
                        if v.used_by > 0 {
                            theme.base()
                        } else {
                            theme.faint_style()
                        },
                    ))
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(size_cell(v.size, measuring, &theme, sym, spinner))
                        .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(Span::styled(
                        v.created
                            .map(|c| format::duration_short(chrono::Utc::now().timestamp() - c))
                            .unwrap_or_else(|| "-".into()),
                        theme.dim(),
                    ))
                    .alignment(Alignment::Right),
                ),
            ];
            if wide {
                cells.push(Cell::from(Span::styled(
                    format::truncate_start(&v.mountpoint, widths[6] as usize),
                    theme.faint_style(),
                )));
            }

            let mut row = Row::new(cells).height(1);
            if let Some(bg) = theme.stripe(position) {
                row = row.style(Style::default().bg(bg));
            }
            Some(row)
        })
        .collect();

    render(frame, app, area, focused, &cols, &constraints, rows);
}

/// The size cell: a number once measured, a spinner while the daemon is
/// walking, and a dash the rest of the time.
///
/// The dash is the honest answer rather than a placeholder — Docker doesn't
/// hand out volume sizes with the listing, so until someone asks for a
/// measurement there is genuinely nothing to show.
fn size_cell(
    size: Option<i64>,
    measuring: bool,
    theme: &Theme,
    sym: crate::ui::theme::Symbols,
    spinner: usize,
) -> Span<'static> {
    match (size, measuring) {
        (Some(bytes), _) => Span::styled(format::bytes_i64(bytes), theme.base()),
        (None, true) => Span::styled(
            sym.spin(spinner / 2).to_string(),
            Style::default().fg(theme.info),
        ),
        (None, false) => Span::styled("-".to_string(), theme.faint_style()),
    }
}

// ---------------------------------------------------------------------------
// Networks
// ---------------------------------------------------------------------------

fn networks(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let wide = area.width >= 84;
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(18),    // name
        Constraint::Length(10), // driver
        Constraint::Length(7),  // scope
        Constraint::Length(5),  // used
    ];
    let mut cols = vec![
        col("", Alignment::Left, None),
        col("NAME", Alignment::Left, Some(Sort::Name)),
        col("DRIVER", Alignment::Left, Some(Sort::Driver)),
        col("SCOPE", Alignment::Left, None),
        col("USED", Alignment::Right, None),
    ];
    if wide {
        constraints.push(Constraint::Percentage(28));
        cols.push(col("SUBNET", Alignment::Left, None));
    }

    let widths = ui::column_widths(area.width, &constraints, SPACING);
    let theme = app.theme.clone();
    let needle = app.filter[app.tab.index()].value.clone();
    let sym = app.symbols;

    let rows: Vec<Row> = app
        .view()
        .iter()
        .enumerate()
        .filter_map(|(position, row)| {
            // These tabs are never grouped, so every row is a resource.
            let ViewRow::Item(index) = *row else {
                return None;
            };
            let n = app.networks.get(index)?;
            let (dot, dot_style) = if n.used_by > 0 {
                (sym.running, Style::default().fg(theme.success))
            } else {
                (sym.stopped, Style::default().fg(theme.faint))
            };

            let mut cells = vec![
                Cell::from(Span::styled(dot, dot_style)),
                name_cell(
                    &theme,
                    format::truncate(&n.name, widths[1] as usize),
                    &needle,
                    n.is_predefined(),
                ),
                Cell::from(Span::styled(
                    format::truncate(&n.driver, widths[2] as usize),
                    theme.dim(),
                )),
                Cell::from(Span::styled(
                    format::truncate(&n.scope, widths[3] as usize),
                    theme.faint_style(),
                )),
                Cell::from(
                    Line::from(Span::styled(
                        if n.used_by > 0 {
                            n.used_by.to_string()
                        } else {
                            "-".into()
                        },
                        if n.used_by > 0 {
                            theme.base()
                        } else {
                            theme.faint_style()
                        },
                    ))
                    .alignment(Alignment::Right),
                ),
            ];
            if wide {
                cells.push(Cell::from(Span::styled(
                    format::truncate(&n.subnets.join(", "), widths[5] as usize),
                    theme.faint_style(),
                )));
            }

            let mut row = Row::new(cells).height(1);
            if let Some(bg) = theme.stripe(position) {
                row = row.style(Style::default().bg(bg));
            }
            Some(row)
        })
        .collect();

    render(frame, app, area, focused, &cols, &constraints, rows);
}
