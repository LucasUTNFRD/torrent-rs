use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Sparkline},
};

use crate::app::App;
use crate::torrent::format_bytes_per_second;

pub fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let stats_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(70),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ])
        .split(area);

    let stats_text = vec![Line::from(vec![
        Span::raw(" Download: "),
        Span::styled(
            format_bytes_per_second(app.stats.total_download_rate),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" | Upload: "),
        Span::styled(
            format_bytes_per_second(app.stats.total_upload_rate),
            Style::default().fg(Color::Red),
        ),
        Span::raw(" | Torrents: "),
        Span::raw(format!(
            "{} DL, {} Seed",
            app.stats.torrents_downloading, app.stats.torrents_seeding
        )),
        Span::raw(" | DHT: "),
        Span::raw(format!("{}", app.stats.dht_nodes.unwrap_or(0))),
    ])];
    let stats_para = Paragraph::new(stats_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Session Stats "),
    );
    f.render_widget(stats_para, stats_chunks[0]);

    let dl_data: Vec<u64> = app.download_history.iter().copied().collect();
    let dl_sparkline = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
                .title(" DL "),
        )
        .data(&dl_data)
        .style(Style::default().fg(Color::Green));
    f.render_widget(dl_sparkline, stats_chunks[1]);

    let ul_data: Vec<u64> = app.upload_history.iter().copied().collect();
    let ul_sparkline = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
                .title(" UL "),
        )
        .data(&ul_data)
        .style(Style::default().fg(Color::Red));
    f.render_widget(ul_sparkline, stats_chunks[2]);
}
