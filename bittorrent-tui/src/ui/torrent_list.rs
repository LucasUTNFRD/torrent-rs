use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem},
};

use crate::app::{App, Mode, Tab};

pub fn draw_torrent_list(f: &mut Frame, app: &mut App, area: Rect) {
    let torrents: Vec<ListItem> = app
        .torrents
        .iter()
        .map(|t| {
            let status = format!("{}", t.state);
            let progress = (t.progress * 100.0) as u64;
            ListItem::new(format!("{:<20} {:>3}% [{}]", t.name, progress, status))
        })
        .collect();

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(" Torrents ")
        .border_style(
            if app.active_tab == Tab::Torrents && app.mode == Mode::Main {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            },
        );

    let list = List::new(torrents)
        .block(list_block)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol(">> ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}
