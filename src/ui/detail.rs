//! The detail pane: everything about the selected resource that doesn't fit in
//! a table row.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{LineGauge, Paragraph, Wrap};

use crate::app::{App, Tab};
use crate::docker::model::Health;
use crate::util::format;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let block = crate::ui::pane(app, crate::ui::title(app, "details", None, false), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 8 || inner.height == 0 {
        return;
    }

    let mut lines = match app.tab {
        Tab::Containers => container_lines(app, inner.width),
        Tab::Images => image_lines(app, inner.width),
        Tab::Volumes => volume_lines(app, inner.width),
        Tab::Networks => network_lines(app, inner.width),
    };

    // The name always leads — it answers "what am I looking at?" — so it is
    // peeled off the front and the gauges slot in underneath it.
    let title_line = if lines.is_empty() {
        Line::raw("")
    } else {
        lines.remove(0)
    };
    frame.render_widget(Paragraph::new(title_line), Rect { height: 1, ..inner });

    // Live gauges only make sense for a running container we have a reading for.
    let gauge_rows = if app.tab == Tab::Containers
        && app
            .selected_container()
            .is_some_and(|c| c.state.is_running())
        && app
            .selected_container()
            .and_then(|c| app.latest_stat(&c.id))
            .is_some()
        && inner.height > 9
    {
        5
    } else {
        2
    };

    if gauge_rows > 2 {
        draw_gauges(
            frame,
            app,
            Rect {
                y: inner.y + 2,
                height: 3,
                ..inner
            },
        );
    }

    let text_area = Rect {
        y: inner.y + gauge_rows,
        height: inner.height.saturating_sub(gauge_rows),
        ..inner
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text_area);
}

fn draw_gauges(frame: &mut Frame, app: &App, area: Rect) {
    let Some(c) = app.selected_container() else {
        return;
    };
    let Some(stat) = app.latest_stat(&c.id) else {
        return;
    };
    let theme = &app.theme;

    // `LineGauge` rather than `Gauge`: it puts the label on the left, which
    // lines up with the field list below instead of floating in the centre.
    let cpu = LineGauge::default()
        .filled_style(Style::default().fg(gauge_color(app, stat.cpu_percent)))
        .unfilled_style(Style::default().fg(theme.border))
        .ratio((stat.cpu_percent / 100.0).clamp(0.0, 1.0))
        .label(Span::styled(
            format!("cpu {:>6.1}%", stat.cpu_percent),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(cpu, Rect { height: 1, ..area });

    let mem_pct = stat.mem_percent().unwrap_or(0.0);
    let mem_label = format!("mem {:>6}", format::bytes(stat.mem_bytes));
    let mem = LineGauge::default()
        .filled_style(Style::default().fg(gauge_color(app, mem_pct)))
        .unfilled_style(Style::default().fg(theme.border))
        .ratio((mem_pct / 100.0).clamp(0.0, 1.0))
        .label(Span::styled(
            mem_label,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(
        mem,
        Rect {
            y: area.y + 1,
            height: 1,
            ..area
        },
    );

    let mut io_spans = vec![];
    // The memory bar shows usage against the limit; spell the limit out here
    // rather than crowding the bar's label.
    if stat.mem_limit > 0 {
        io_spans.push(Span::styled("of ", app.theme.faint_style()));
        io_spans.push(Span::styled(
            format!("{}   ", format::bytes(stat.mem_limit)),
            app.theme.dim(),
        ));
    }
    io_spans.extend([
        Span::styled("net ", app.theme.faint_style()),
        Span::styled(format!("↓{} ", format::bytes(stat.net_rx)), app.theme.dim()),
        Span::styled(format!("↑{}", format::bytes(stat.net_tx)), app.theme.dim()),
        Span::styled("   disk ", app.theme.faint_style()),
        Span::styled(
            format!("↓{} ", format::bytes(stat.block_read)),
            app.theme.dim(),
        ),
        Span::styled(
            format!("↑{}", format::bytes(stat.block_write)),
            app.theme.dim(),
        ),
        Span::styled("   pids ", app.theme.faint_style()),
        Span::styled(stat.pids.to_string(), app.theme.dim()),
    ]);
    let io = Line::from(io_spans);
    frame.render_widget(
        Paragraph::new(io),
        Rect {
            y: area.y + 2,
            height: 1,
            ..area
        },
    );
}

fn gauge_color(app: &App, percent: f64) -> ratatui::style::Color {
    if percent >= 80.0 {
        app.theme.danger
    } else if percent >= 50.0 {
        app.theme.warn
    } else {
        app.theme.success
    }
}

// ---------------------------------------------------------------------------
// Per-tab content
// ---------------------------------------------------------------------------

fn container_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    // A stack header is selected rather than any one container, so describe the
    // stack instead of leaving the pane blank.
    if app.on_group() {
        return stack_lines(app);
    }
    let Some(c) = app.selected_container() else {
        return vec![empty(app)];
    };
    let mut out = vec![heading(app, &c.name)];

    out.push(field(app, "id", format::short_id(&c.id)));
    out.push(field(app, "image", c.image.clone()));
    // The tag can move under a running container, so the resolved image id is
    // worth showing next to it.
    if !c.image_id.is_empty() {
        out.push(field(app, "image id", format::short_id(&c.image_id)));
    }
    out.push(field(app, "state", c.status.clone()));

    if c.health != Health::None {
        let (text, color) = match c.health {
            Health::Healthy => ("healthy", app.theme.success),
            Health::Unhealthy => ("unhealthy", app.theme.danger),
            Health::Starting => ("starting", app.theme.warn),
            Health::None => ("", app.theme.text),
        };
        out.push(colored_field(app, "health", text.to_string(), color));
    }

    out.push(field(app, "created", format::age_from_epoch(c.created)));

    if let (Some(stack), Some(service)) = (&c.stack, &c.service) {
        out.push(field(app, "stack", format!("{stack} / {service}")));
    }

    out.push(Line::raw(""));
    out.push(section(app, "ports"));
    if c.ports.is_empty() {
        out.push(bullet(app, "none published".into(), true));
    } else {
        for p in &c.ports {
            out.push(bullet(app, p.display(), p.public.is_none()));
        }
    }

    if !c.networks.is_empty() {
        out.push(Line::raw(""));
        out.push(section(app, "networks"));
        for n in &c.networks {
            out.push(bullet(app, n.clone(), false));
        }
    }

    if !c.mounts.is_empty() {
        out.push(Line::raw(""));
        out.push(section(app, "mounts"));
        for m in &c.mounts {
            out.push(bullet(
                app,
                format::truncate_start(m, width.saturating_sub(4) as usize),
                false,
            ));
        }
    }

    if !c.command.is_empty() {
        out.push(Line::raw(""));
        out.push(section(app, "command"));
        out.push(bullet(app, c.command.clone(), true));
    }

    out
}

/// What a whole stack looks like: its services, and what the lot of them are
/// costing. The aggregate is the reason to look here rather than at the rows —
/// "this project is using 3 GB" isn't visible anywhere else.
fn stack_lines(app: &App) -> Vec<Line<'static>> {
    let Some(group) = app.current_group() else {
        return vec![empty(app)];
    };
    let members: Vec<&crate::docker::model::ContainerRow> = app
        .containers
        .iter()
        .filter(|c| c.stack.clone().unwrap_or_default() == group.key)
        .collect();

    let mut out = vec![heading(app, group.label())];
    out.push(field(
        app,
        "services",
        format!(
            "{} {}",
            group.items,
            if group.collapsed { "(folded)" } else { "" }
        )
        .trim_end()
        .to_string(),
    ));
    out.push(colored_field(
        app,
        "running",
        format!("{}/{}", group.running, group.items),
        if group.running == group.items {
            app.theme.success
        } else {
            app.theme.warn
        },
    ));

    // Only running members report stats, so this is the stack's live cost.
    let stats: Vec<&crate::docker::stats::StatSample> = members
        .iter()
        .filter_map(|c| app.latest_stat(&c.id))
        .collect();
    if !stats.is_empty() {
        let cpu: f64 = stats.iter().map(|s| s.cpu_percent).sum();
        let mem: u64 = stats.iter().map(|s| s.mem_bytes).sum();
        out.push(field(app, "cpu", format!("{cpu:.1}%")));
        out.push(field(app, "memory", format::bytes(mem)));
    }

    out.push(Line::raw(""));
    out.push(section(app, "services"));
    for c in &members {
        out.push(bullet(
            app,
            format!("{}  {}", c.service_label(), state_summary(c)),
            !c.state.is_running(),
        ));
    }

    out
}

/// The shortest true thing to say about a container's state, for the stack list.
fn state_summary(c: &crate::docker::model::ContainerRow) -> String {
    if c.status.is_empty() {
        return c.state.label().to_string();
    }
    c.status.split(" (").next().unwrap_or(&c.status).to_string()
}

fn image_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(i) = app.selected_image() else {
        return vec![empty(app)];
    };
    let mut out = vec![heading(app, &i.reference())];
    out.push(field(app, "id", format::short_id(&i.id)));
    out.push(field(app, "size", format::bytes_i64(i.size)));
    if i.shared_size > 0 {
        out.push(field(app, "shared", format::bytes_i64(i.shared_size)));
    }
    out.push(field(app, "created", format::age_from_epoch(i.created)));
    out.push(field(
        app,
        "in use by",
        if i.containers > 0 {
            format!("{} container(s)", i.containers)
        } else {
            "nothing".into()
        },
    ));

    if i.all_tags.len() > 1 {
        out.push(Line::raw(""));
        out.push(section(app, "tags"));
        for t in &i.all_tags {
            out.push(bullet(
                app,
                format::truncate_start(t, width.saturating_sub(4) as usize),
                false,
            ));
        }
    }
    out
}

fn volume_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(v) = app.selected_volume() else {
        return vec![empty(app)];
    };
    let mut out = vec![heading(app, &v.name)];
    out.push(field(app, "driver", v.driver.clone()));
    out.push(field(
        app,
        "used by",
        if v.used_by > 0 {
            format!("{} container(s)", v.used_by)
        } else {
            "nothing".into()
        },
    ));
    if let Some(size) = v.size {
        out.push(field(app, "size", format::bytes_i64(size)));
    }
    if let Some(created) = v.created {
        out.push(field(app, "created", format::age_from_epoch(created)));
    }
    if let Some(project) = &v.compose_project {
        out.push(field(app, "compose", project.clone()));
    }
    out.push(Line::raw(""));
    out.push(section(app, "mountpoint"));
    out.push(bullet(
        app,
        format::truncate_start(&v.mountpoint, width.saturating_sub(4) as usize),
        true,
    ));
    out
}

fn network_lines(app: &App, _width: u16) -> Vec<Line<'static>> {
    let Some(n) = app.selected_network() else {
        return vec![empty(app)];
    };
    let mut out = vec![heading(app, &n.name)];
    out.push(field(app, "id", format::short_id(&n.id)));
    out.push(field(app, "driver", n.driver.clone()));
    out.push(field(app, "scope", n.scope.clone()));
    out.push(field(app, "internal", yes_no(n.internal)));
    out.push(field(app, "ipv6", yes_no(n.ipv6)));
    out.push(field(
        app,
        "used by",
        if n.used_by > 0 {
            format!("{} container(s)", n.used_by)
        } else {
            "nothing".into()
        },
    ));
    if let Some(created) = n.created {
        out.push(field(app, "created", format::age_from_epoch(created)));
    }
    if let Some(project) = &n.compose_project {
        out.push(field(app, "compose", project.clone()));
    }
    if !n.subnets.is_empty() {
        out.push(Line::raw(""));
        out.push(section(app, "subnets"));
        for s in &n.subnets {
            out.push(bullet(app, s.clone(), false));
        }
    }
    if n.is_predefined() {
        out.push(Line::raw(""));
        out.push(bullet(
            app,
            "built-in network, cannot be removed".into(),
            true,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Line builders
// ---------------------------------------------------------------------------

fn yes_no(b: bool) -> String {
    if b { "yes".into() } else { "no".into() }
}

fn empty(app: &App) -> Line<'static> {
    Line::from(Span::styled("nothing selected", app.theme.dim()))
}

fn heading(app: &App, text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(app.theme.primary)
            .add_modifier(Modifier::BOLD),
    ))
}

fn section(app: &App, text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_uppercase(),
        Style::default()
            .fg(app.theme.faint)
            .add_modifier(Modifier::BOLD),
    ))
}

fn field(app: &App, label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<9}"), app.theme.faint_style()),
        Span::styled(value, app.theme.base()),
    ])
}

fn colored_field(
    app: &App,
    label: &str,
    value: String,
    color: ratatui::style::Color,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<9}"), app.theme.faint_style()),
        Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn bullet(app: &App, text: String, muted: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", app.symbols.bullet), app.theme.faint_style()),
        Span::styled(
            text,
            if muted {
                app.theme.dim()
            } else {
                app.theme.base()
            },
        ),
    ])
}
