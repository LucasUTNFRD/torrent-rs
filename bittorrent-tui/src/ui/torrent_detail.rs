use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs},
};

use crate::app::{App, Mode, Tab};
use crate::torrent::{format_bytes, format_bytes_per_second};

pub fn draw_torrent_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let details_block = Block::default()
        .borders(Borders::ALL)
        .title(" Details ")
        .border_style(
            if app.active_tab == Tab::Details && app.mode == Mode::Main {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            },
        );

    if let Some(details) = &app.selected_torrent {
        let details_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
            ])
            .split(area.inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 1,
            }));

        // Render the outer block
        f.render_widget(details_block, area);

        let titles = vec!["Peers", "Trackers", "Files"];
        let tabs = Tabs::new(titles)
            .block(Block::default().borders(Borders::BOTTOM))
            .select(app.details_tab)
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        f.render_widget(tabs, details_chunks[0]);

        match app.details_tab {
            0 => {
                let rows: Vec<Row> = details
                    .peers
                    .iter()
                    .map(|p| {
                        Row::new(vec![
                            Cell::from(p.ip.clone()),
                            Cell::from(format_bytes_per_second(p.rate_down / 8)),
                            Cell::from(format_bytes_per_second(p.rate_up / 8)),
                            Cell::from(p.client_id.clone()),
                        ])
                    })
                    .collect();
                let table = Table::new(
                    rows,
                    [
                        Constraint::Percentage(30),
                        Constraint::Percentage(20),
                        Constraint::Percentage(20),
                        Constraint::Percentage(30),
                    ],
                )
                .header(
                    Row::new(vec!["IP", "Down", "Up", "Client"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(Block::default());
                f.render_widget(table, details_chunks[1]);
            }
            1 => {
                let rows: Vec<Row> = details
                    .trackers
                    .iter()
                    .map(|t| {
                        Row::new(vec![
                            Cell::from(t.url.clone()),
                            Cell::from(t.error.as_deref().unwrap_or("OK")),
                        ])
                    })
                    .collect();
                let table = Table::new(
                    rows,
                    [Constraint::Percentage(70), Constraint::Percentage(30)],
                )
                .header(
                    Row::new(vec!["URL", "Status"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(Block::default());
                f.render_widget(table, details_chunks[1]);
            }
            2 => {
                let rows: Vec<Row> = details
                    .files
                    .iter()
                    .map(|fi| {
                        Row::new(vec![
                            Cell::from(fi.path.clone()),
                            Cell::from(format_bytes(fi.size)),
                            Cell::from(format!("{:.1}%", fi.progress * 100.0)),
                        ])
                    })
                    .collect();
                let table = Table::new(
                    rows,
                    [
                        Constraint::Percentage(60),
                        Constraint::Percentage(20),
                        Constraint::Percentage(20),
                    ],
                )
                .header(
                    Row::new(vec!["Path", "Size", "Progress"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(Block::default());
                f.render_widget(table, details_chunks[1]);
            }
            _ => unreachable!(),
        }
    } else {
        let empty_para = Paragraph::new("Select a torrent to see details").block(details_block);
        f.render_widget(empty_para, area);
    }
}
