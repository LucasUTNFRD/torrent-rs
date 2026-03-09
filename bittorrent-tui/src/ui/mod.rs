pub mod torrent_list;
pub mod torrent_detail;
pub mod status_bar;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, Mode};
use crate::ui::status_bar::draw_status_bar;
use crate::ui::torrent_detail::draw_torrent_detail;
use crate::ui::torrent_list::draw_torrent_list;

pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn render(f: &mut Frame, app: &mut App) {
    let size = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(3), // Footer / Stats
        ])
        .split(size);

    // Header
    let header = Block::default()
        .borders(Borders::ALL)
        .title(" BitTorrent TUI ");
    f.render_widget(header, chunks[0]);

    let header_text = vec![
        Line::from(vec![
            Span::styled(" Q", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": Quit | "),
            Span::styled(" J/K", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": Navigate | "),
            Span::styled(" A", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": Add Magnet | "),
            Span::styled(" F", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": Add File | "),
            Span::styled(" X", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": Remove | "),
            Span::styled(" Tab", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": Cycle Details "),
        ])
    ];
    let header_para = Paragraph::new(header_text).block(Block::default().borders(Borders::NONE));
    f.render_widget(header_para, chunks[0].inner(ratatui::layout::Margin { vertical: 1, horizontal: 1 }));

    // Main Content
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ])
        .split(chunks[1]);

    draw_torrent_list(f, app, main_chunks[0]);
    draw_torrent_detail(f, app, main_chunks[1]);

    // Modals & Overlays
    if let Some((msg, time)) = &app.status_message {
        if time.elapsed() < std::time::Duration::from_secs(3) {
            let status_block = Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(ratatui::style::Color::Yellow));
            let status_para = Paragraph::new(msg.as_str()).block(status_block);
            let area = centered_rect(40, 10, f.area());
            f.render_widget(ratatui::widgets::Clear, area);
            f.render_widget(status_para, area);
        }
    }

    match app.mode {
        Mode::AddMagnet => {
            let area = centered_rect(60, 20, f.area());
            let block = Block::default().title(" Add Magnet URI ").borders(Borders::ALL);
            let input_para = Paragraph::new(app.input.as_str()).block(block);
            f.render_widget(ratatui::widgets::Clear, area);
            f.render_widget(input_para, area);
        }
        Mode::FileBrowser => {
            let area = centered_rect(80, 80, f.area());
            let items: Vec<ratatui::widgets::ListItem> = app.file_browser.entries.iter().map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let name = e.file_name().to_string_lossy().to_string();
                if is_dir {
                    ratatui::widgets::ListItem::new(format!(" {}", name)).style(Style::default().fg(ratatui::style::Color::Blue))
                } else {
                    ratatui::widgets::ListItem::new(format!(" {}", name))
                }
            }).collect();
            let block = Block::default()
                .title(format!(" File Browser: {} ", app.file_browser.current_dir.display()))
                .borders(Borders::ALL);
            let list = ratatui::widgets::List::new(items)
                .block(block)
                .highlight_style(Style::default().bg(ratatui::style::Color::DarkGray));
            f.render_widget(ratatui::widgets::Clear, area);
            f.render_stateful_widget(list, area, &mut app.file_browser.state);
        }
        _ => {}
    }

    draw_status_bar(f, app, chunks[2]);
}