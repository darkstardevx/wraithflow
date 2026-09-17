//! All rendering. Real I/O-adjacent (everything here takes a `Frame`),
//! untested directly -- same "real I/O stays real" split as
//! `wf-proxy`'s own accept loop; the decisions behind what gets shown
//! live in `app.rs` and are tested there.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Row, Sparkline, Table, TableState, Wrap,
};
use ratatui::Frame;

use crate::app::{aggregate, sparkline_chars, App, Mode, PipelineStat};

/// Every color `draw` needs, resolved once from `cybercore` (or plain
/// ANSI fallbacks under `--no-color`) rather than re-queried per frame.
#[derive(Clone, Copy)]
pub struct Palette {
    pub healthy: Color,
    pub error: Color,
    pub muted: Color,
    pub accent: Color,
    pub border: Color,
    pub text: Color,
}

fn row_style(pipeline: &PipelineStat, palette: &Palette) -> Style {
    if pipeline.errors_total > 0 {
        Style::new().fg(palette.error)
    } else if pipeline.connections_active > 0 {
        Style::new().fg(palette.healthy)
    } else {
        Style::new().fg(palette.muted)
    }
}

pub fn draw(frame: &mut Frame, app: &App, table_state: &mut TableState, palette: &Palette) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // aggregate summary
            Constraint::Min(3),    // table
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_summary(frame, app, chunks[0], palette);
    draw_table(frame, app, table_state, chunks[1], palette);
    draw_footer(frame, app, chunks[2], palette);

    match app.mode {
        Mode::Help => draw_help_popup(frame, area, palette),
        Mode::Detail => draw_detail_popup(frame, app, area, palette),
        Mode::Normal => {}
    }
}

fn draw_summary(frame: &mut Frame, app: &App, area: Rect, palette: &Palette) {
    let total = aggregate(&app.pipelines);
    let text = format!(
        " WraithFlow — {} pipeline(s) — active={} total={} in={}B out={}B errors={} ",
        app.pipelines.len(),
        total.connections_active,
        total.connections_total,
        total.bytes_in,
        total.bytes_out,
        total.errors_total
    );
    let style = if total.errors_total > 0 {
        Style::new().fg(palette.error).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(palette.accent).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(Paragraph::new(Span::styled(text, style)), area);
}

fn draw_table(
    frame: &mut Frame,
    app: &App,
    table_state: &mut TableState,
    area: Rect,
    palette: &Palette,
) {
    table_state.select(if app.pipelines.is_empty() {
        None
    } else {
        Some(app.selected)
    });

    let sort_arrow = if app.sort_desc { "▼" } else { "▲" };
    let header = Row::new(vec![
        format!(
            "Pipeline{}",
            if app.sort_field == crate::app::SortField::Name {
                sort_arrow
            } else {
                ""
            }
        ),
        "Active/Total".to_string(),
        "In".to_string(),
        "Out".to_string(),
        "Errors".to_string(),
        "History".to_string(),
    ])
    .style(
        Style::new()
            .fg(palette.text)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = app
        .pipelines
        .iter()
        .map(|p| {
            let history = app
                .history
                .get(&p.name)
                .map(|h| h.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            Row::new(vec![
                p.name.clone(),
                format!("{}/{}", p.connections_active, p.connections_total),
                format!("{}B", p.bytes_in),
                format!("{}B", p.bytes_out),
                p.errors_total.to_string(),
                sparkline_chars(&history, 12),
            ])
            .style(row_style(p, palette))
        })
        .collect();

    let widths = [
        Constraint::Fill(1),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(14),
    ];

    let title = match &app.last_error {
        Some(e) => format!(" disconnected ({e}) "),
        None if app.paused => " PAUSED ".to_string(),
        None => " live ".to_string(),
    };

    let table = Table::new(rows, widths)
        .header(header)
        // Both fg and bg set explicitly: a highlight style that only
        // sets bg leaves fg to fall through from the row's own
        // health-based color, and a muted (idle) row's fg matches a
        // muted highlight bg exactly -- invisible selected text,
        // confirmed live. Setting both guarantees contrast regardless
        // of which row is selected.
        .row_highlight_style(
            Style::new()
                .bg(palette.accent)
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ")
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(palette.border))
                .title(title),
        );

    frame.render_stateful_widget(table, area, table_state);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect, palette: &Palette) {
    let pause_hint = if app.paused { "p resume" } else { "p pause" };
    let text = format!(
        " q quit | ↑/k ↓/j move | s sort ({}) | r reverse | {} | Enter detail | ? help ",
        app.sort_field.label(),
        pause_hint
    );
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::new().fg(palette.muted))),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_help_popup(frame: &mut Frame, area: Rect, palette: &Palette) {
    let popup = centered_rect(50, 50, area);
    frame.render_widget(Clear, popup);
    let lines = [
        "q, Esc      quit",
        "↑/k, ↓/j    move selection",
        "s           cycle sort field",
        "r           reverse sort direction",
        "p           pause/resume auto-refresh",
        "Enter       show detail + history for the selected pipeline",
        "?           toggle this help",
        "Esc/Enter/? close a popup",
    ]
    .join("\n");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.accent))
        .title(Span::styled(" Help ", Style::new().fg(palette.accent)));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_detail_popup(frame: &mut Frame, app: &App, area: Rect, palette: &Palette) {
    let popup = centered_rect(70, 60, area);
    frame.render_widget(Clear, popup);

    let Some(p) = app.selected_pipeline() else {
        return;
    };
    let title = format!(" {} — Enter/Esc to close ", p.name);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.accent))
        .title(Span::styled(title, Style::new().fg(palette.accent)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(3)])
        .split(inner);

    let summary = format!(
        "active/total: {}/{}\nbytes in:     {}B\nbytes out:    {}B\nerrors:       {}",
        p.connections_active, p.connections_total, p.bytes_in, p.bytes_out, p.errors_total
    );
    frame.render_widget(
        Paragraph::new(summary).wrap(Wrap { trim: false }),
        layout[0],
    );

    let history = app.selected_history();
    let sparkline = Sparkline::default()
        .data(history)
        .style(Style::new().fg(palette.healthy))
        .block(Block::default().title("throughput (bytes in+out per poll)"));
    frame.render_widget(sparkline, layout[1]);
}
