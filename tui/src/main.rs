mod app;
mod command;

use std::{
    io::{self, Stdout},
    time::Duration,
};

use anyhow::Result;
use app::{App, DetailTab, View};
use bittorrent_core::{
    DirectionSnapshot, Session, SessionConfig, TorrentProgress, TorrentState, TrackerState,
    events::SessionEvent, types::TorrentId,
};

use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs},
};
use tokio::time;

enum DialogState {
    None,
    AddTorrent(String),
    ConfirmRemove(TorrentId),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let config = SessionConfig::default();
    let session = Session::new(config);
    let mut app = App::new(session);

    let res = run_app(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        anyhow::bail!(err);
    }
    Ok(())
}

async fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let mut events = EventStream::new();
    let mut session_events = app.session.subscribe();
    let mut ticker = time::interval(Duration::from_millis(250));
    let mut dialog: DialogState = DialogState::None;

    loop {
        terminal.draw(|f| ui(f, app, &dialog))?;

        tokio::select! {
            Some(event) = events.next() => {
                let event = event?;
                if let Event::Key(key) = event {
                    if key.kind != KeyEventKind::Press { continue; }
                    handle_key_event(key, app, &mut dialog).await?;
                }
            }
            Ok(event) = session_events.recv() => {
                handle_session_event(event, app).await;
            }
            _ = ticker.tick() => {
                app.tick().await;
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

async fn handle_key_event(
    key: KeyEvent,
    app: &mut App,
    dialog: &mut DialogState,
) -> Result<()> {
    match dialog {
        DialogState::AddTorrent(input) => {
            let mut s = std::mem::take(input);
            handle_add_dialog_key(key, &mut s, app, dialog).await;
            if let DialogState::AddTorrent(modified_input) = dialog {
                *modified_input = s;
            }
            return Ok(());
        }
        DialogState::ConfirmRemove(id) => {
            let id = *id;
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    match app.session.remove_torrent(id).await {
                        Ok(()) => app.remove_torrent_from_state(id),
                        Err(e) => app.set_error(e),
                    }
                    *dialog = DialogState::None;
                }
                _ => {
                    *dialog = DialogState::None;
                }
            }
            return Ok(());
        }
        DialogState::None => {}
    }

    if let View::Detail { .. } = app.view {
        handle_detail_key(key, app);
        return Ok(());
    }

    match key.code {
        KeyCode::Char('q') => {
            app.session.shutdown().await?;
            app.quit();
            Ok(())
        }
        KeyCode::Char('a') => {
            *dialog = DialogState::AddTorrent(String::new());
            Ok(())
        }
        KeyCode::Char('d') => {
            if let Some(id) = app.selected_id() {
                *dialog = DialogState::ConfirmRemove(id);
            }
            Ok(())
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !app.torrent_order.is_empty() {
                app.selected_index = (app.selected_index + 1) % app.torrent_order.len();
            }
            Ok(())
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if !app.torrent_order.is_empty() {
                let len = app.torrent_order.len();
                app.selected_index = (app.selected_index + len - 1) % len;
            }
            Ok(())
        }
        KeyCode::Enter => {
            if let Some(id) = app.selected_id() {
                let _ = app.open_detail(id).await;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn handle_detail_key(key: KeyEvent, app: &mut App) {
    let View::Detail { tab, .. } = app.view else { return };
    let mut new_tab = tab;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.close_detail();
            return;
        }
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
            new_tab = tab.next();
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
            new_tab = tab.prev();
        }
        KeyCode::Down | KeyCode::Char('j') => match tab {
            DetailTab::Files => {
                let mut state = app.detail.files_table_state.get();
                let i = match state.selected() {
                    Some(i) => if app.detail.files.is_empty() { 0 } else { (i + 1).min(app.detail.files.len() - 1) },
                    None => 0,
                };
                if !app.detail.files.is_empty() {
                    state.select(Some(i));
                }
                app.detail.files_table_state.set(state);
            }
            DetailTab::Peers => {
                let mut state = app.detail.peers_table_state.get();
                let i = match state.selected() {
                    Some(i) => if app.detail.peers.is_empty() { 0 } else { (i + 1).min(app.detail.peers.len() - 1) },
                    None => 0,
                };
                if !app.detail.peers.is_empty() {
                    state.select(Some(i));
                }
                app.detail.peers_table_state.set(state);
            }
            _ => {}
        },
        KeyCode::Up | KeyCode::Char('k') => match tab {
            DetailTab::Files => {
                let mut state = app.detail.files_table_state.get();
                let i = match state.selected() {
                    Some(i) => i.saturating_sub(1),
                    None => if app.detail.files.is_empty() { 0 } else { app.detail.files.len() - 1 },
                };
                if !app.detail.files.is_empty() {
                    state.select(Some(i));
                }
                app.detail.files_table_state.set(state);
            }
            DetailTab::Peers => {
                let mut state = app.detail.peers_table_state.get();
                let i = match state.selected() {
                    Some(i) => i.saturating_sub(1),
                    None => if app.detail.peers.is_empty() { 0 } else { app.detail.peers.len() - 1 },
                };
                if !app.detail.peers.is_empty() {
                    state.select(Some(i));
                }
                app.detail.peers_table_state.set(state);
            }
            _ => {}
        },
        _ => {}
    }

    if let View::Detail { ref mut tab, .. } = app.view {
        *tab = new_tab;
    }
}

async fn handle_add_dialog_key(
    key: KeyEvent,
    input: &mut String,
    app: &mut App,
    dialog: &mut DialogState,
) {
    match key.code {
        KeyCode::Esc => *dialog = DialogState::None,
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char(c) => {
            input.push(c);
        }
        KeyCode::Enter => {
            let value = input.trim();
            if !value.is_empty() {
                let res = if value.starts_with("magnet:") {
                    app.session.add_magnet(value).await
                } else {
                    app.session.add_torrent(value).await
                };
                match res {
                    Ok(_) => app.set_status("Torrent added successfully"),
                    Err(e) => app.set_error(e),
                }
            }
            *dialog = DialogState::None;
        }
        _ => {}
    }
}

async fn handle_session_event(
    event: SessionEvent,
    app: &mut App,
) {
    match event {
        SessionEvent::TorrentAdded(id) => {
            let _ = app.add_torrent_to_state(id).await;
        }
        SessionEvent::TorrentRemoved(id) => {
            app.remove_torrent_from_state(id);
        }
        SessionEvent::TorrentCompleted(_id) => {}
        _ => {}
    }
}

fn ui(f: &mut Frame, app: &App, dialog: &DialogState) {
    let mut dialog_len = 0;
    if !matches!(dialog, DialogState::None) {
        dialog_len = 3;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Status bar
            Constraint::Length(3), // Footer
            Constraint::Length(dialog_len),
        ])
        .split(f.area());

    render_header(f, chunks[0], app);

    match &app.view {
        View::List => render_torrent_list(f, chunks[1], app),
        View::Detail { id, tab } => render_detail_view(f, chunks[1], app, *id, *tab),
    }

    render_status_bar(f, chunks[2], app);
    render_footer(f, chunks[3]);
    if dialog_len > 0 {
        render_dialog(f, chunks[4], app, dialog);
    }
}

fn render_header(f: &mut Frame, area: Rect, _app: &App) {
    let title = Paragraph::new("Torrent-RS TUI").block(
        Block::default()
            .borders(Borders::ALL)
            .title("Info"),
    );
    f.render_widget(title, area);
}

fn render_torrent_list(f: &mut Frame, area: Rect, app: &App) {
    use ratatui::widgets::{List, ListItem, ListState};

    let items: Vec<ListItem> = app
        .torrent_order
        .iter()
        .filter_map(|id| {
            let progress = app.progress.get(id)?;
            Some(ListItem::new(torrent_row(progress)))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Torrents"),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    let mut list_state = ListState::default();
    if !app.torrent_order.is_empty() {
        list_state.select(Some(app.selected_index));
    }
    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let (downloading, seeding, paused) = app.progress.values().fold(
        (0u32, 0u32, 0u32),
        |(dl, sd, pa), p| match p.state {
            TorrentState::Downloading      => (dl + 1, sd, pa),
            TorrentState::Seeding          => (dl, sd + 1, pa),
            TorrentState::Paused           => (dl, sd, pa + 1),
            _                              => (dl, sd, pa),
        },
    );
    let total = app.torrent_order.len();

    let text = Line::from(vec![
        Span::raw(format!(" Torrents: {} ", total)),
        Span::raw("("),
        Span::styled(
            format!("Downloading: {}", downloading),
            Style::default().fg(Color::Blue),
        ),
        Span::raw(", "),
        Span::styled(
            format!("Seeding: {}", seeding),
            Style::default().fg(Color::Green),
        ),
        Span::raw(", "),
        Span::styled(
            format!("Paused: {}", paused),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(")"),
    ]);

    f.render_widget(
        Paragraph::new(text).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn render_footer(f: &mut Frame, area: Rect) {
    let controls = " 'a': add  'd': remove  j/k or arrows: navigate  'Enter': detail  'q': quit ";
    let footer =
        Paragraph::new(controls).block(Block::default().borders(Borders::ALL).title("Controls"));
    f.render_widget(footer, area);
}

fn render_dialog(f: &mut Frame, area: Rect, app: &App, dialog: &DialogState) {
    let (block, text, fg) = match dialog {
        DialogState::AddTorrent(input) => (
            Block::default()
                .borders(Borders::ALL)
                .title("Add Torrent")
                .style(Style::default().fg(Color::Yellow)),
            format!("Path or magnet URI: {}_", input),
            Color::Yellow,
        ),
        DialogState::ConfirmRemove(id) => {
            let name = app
                .progress
                .get(id)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            (
                Block::default()
                    .borders(Borders::ALL)
                    .title("Confirm Remove")
                    .style(Style::default().fg(Color::Red)),
                format!("Remove \"{}\"? [y] yes  [Esc] cancel", name),
                Color::Red,
            )
        }
        DialogState::None => {
            match &app.status {
                app::Status::Error(err) => (
                    Block::default().borders(Borders::ALL).title("Status"),
                    format!("Error: {}", err),
                    Color::Red,
                ),
                app::Status::Info(msg) => (
                    Block::default().borders(Borders::ALL).title("Status"),
                    msg.clone(),
                    Color::Green,
                ),
                app::Status::None => (
                    Block::default().borders(Borders::ALL),
                    String::new(),
                    Color::White,
                ),
            }
        }
    };

    let paragraph = Paragraph::new(text)
        .block(block)
        .style(Style::default().fg(fg));
    f.render_widget(paragraph, area);
}

fn torrent_row(progress: &TorrentProgress) -> Line<'static> {
    let (badge, badge_color) = match &progress.state {
        TorrentState::FetchingMetadata => (" MT ", Color::DarkGray),
        TorrentState::Checking => (" CK ", Color::Yellow),
        TorrentState::Downloading => (" DL ", Color::Blue),
        TorrentState::Seeding => (" SD ", Color::Green),
        TorrentState::Paused => (" PA ", Color::Gray),
        TorrentState::Error(_) => (" ER ", Color::Red),
        TorrentState::Finished => (" FN ", Color::Cyan),
    };

    let name = truncate(&progress.name, 36);

    let progress_bar = if progress.total_bytes == 0 {
        "[????????????]".to_string()
    } else {
        let pct = (progress.downloaded_bytes as f64 / progress.total_bytes as f64) * 100.0;
        progress_bar(pct, 12)
    };

    let dl_rate = fmt_rate(progress.download_rate);
    let ul_rate = fmt_rate(progress.upload_rate);
    let peers = progress.connected_peers;
    let eta = fmt_eta(progress.eta_seconds);

    let dim_style = match &progress.state {
        TorrentState::Downloading | TorrentState::Seeding => Style::default(),
        _ => Style::default().fg(Color::DarkGray),
    };

    let spans = vec![
        Span::styled(
            badge,
            Style::default()
                .bg(badge_color)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::raw(name),
        Span::raw("  "),
        Span::raw(progress_bar),
        Span::raw("  dl:"),
        Span::styled(dl_rate, dim_style),
        Span::raw("  ul:"),
        Span::styled(ul_rate, dim_style),
        Span::raw("  peers:"),
        Span::raw(format!("{:>3}", peers)),
        Span::raw("  "),
        Span::styled(eta, dim_style),
    ];

    Line::from(spans)
}

fn render_detail_view(
    f: &mut Frame,
    area: Rect,
    app: &App,
    id: TorrentId,
    tab: DetailTab,
) {
    let chunks = Layout::vertical([
        Constraint::Length(2),  // tab bar
        Constraint::Min(0),     // tab content
    ]).split(area);

    render_tab_bar(f, chunks[0], tab);

    match tab {
        DetailTab::Overview => render_overview(f, chunks[1], app, id),
        DetailTab::Files    => render_files(f, chunks[1], app),
        DetailTab::Peers    => render_peers(f, chunks[1], app),
        DetailTab::Trackers => render_trackers(f, chunks[1], app),
    }
}

fn render_tab_bar(f: &mut Frame, area: Rect, active: DetailTab) {
    let tabs = Tabs::new(DetailTab::titles())
        .select(active as usize)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::raw(" "));

    f.render_widget(tabs, area);
}

fn render_overview(f: &mut Frame, area: Rect, app: &App, _id: TorrentId) {
    let chunks = Layout::vertical([
        Constraint::Min(0),      // Details block
        Constraint::Length(7),   // bottom row
    ]).split(area);

    render_overview_details(f, chunks[0], app);

    let bottom = Layout::horizontal([
        Constraint::Percentage(20),   // State
        Constraint::Percentage(55),   // Dates
        Constraint::Percentage(25),   // Peers
    ]).split(chunks[1]);

    // Simple placeholders for the bottom row if not fully implemented in guides
    let state_block = Block::default().borders(Borders::ALL).title(" State ");
    let dates_block = Block::default().borders(Borders::ALL).title(" Dates ");
    let peers_block = Block::default().borders(Borders::ALL).title(" Peers Summary ");
    f.render_widget(state_block, bottom[0]);
    f.render_widget(dates_block, bottom[1]);
    f.render_widget(peers_block, bottom[2]);
}

fn render_overview_details(f: &mut Frame, area: Rect, app: &App) {
    let meta = app.detail.meta.as_ref();

    let label_style = Style::default().fg(Color::DarkGray);
    let rows: Vec<Line> = vec![
        kv_line("Name",   meta.map(|m| m.name.as_str()).unwrap_or("…"), label_style),
        kv_line("Hash",   meta.map(|m| m.info_hash.to_string()).as_deref().unwrap_or("…"), label_style),
        kv_line("Size",   meta.map(|m| fmt_size(m.total_bytes)).as_deref().unwrap_or("…"), label_style),
        kv_line("Pieces", meta.map(|m| format!("{} @ {}", m.num_pieces, fmt_size(m.piece_length as u64))).as_deref().unwrap_or("…"), label_style),
        kv_line("Privacy", meta.map(|m| if m.is_private { "Private" } else { "Public torrent" }).unwrap_or("…"), label_style),
    ];

    let text = Text::from(rows);
    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Details ")),
        area,
    );
}

fn kv_line<'a>(label: &'static str, value: &str, label_style: Style) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{:>12}: ", label), label_style),
        Span::raw(value.to_string()),
    ])
}

fn render_files(f: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app.detail.files.iter().map(|fi| {
        let pct = if fi.size_bytes > 0 {
            (fi.downloaded_bytes as f64 / fi.size_bytes as f64) * 100.0
        } else { 100.0 };
        Row::new(vec![
            Cell::from(fi.path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string()),
            Cell::from(fmt_size(fi.size_bytes)),
            Cell::from(format!("{:.1}%", pct))
                .style(Style::default().fg(if pct >= 100.0 { Color::Green } else { Color::Blue })),
        ])
    }).collect();

    let widths = [Constraint::Min(30), Constraint::Length(12), Constraint::Length(8)];
    let table = Table::new(rows, widths)
        .header(Row::new(["Path", "Size", "Progress"])
            .style(Style::default().add_modifier(Modifier::BOLD))
            .bottom_margin(1))
        .block(Block::default().borders(Borders::ALL).title(" Files "))
        .row_highlight_style(Style::default().bg(Color::DarkGray));

    let mut state = app.detail.files_table_state.get();
    f.render_stateful_widget(table, area, &mut state);
    app.detail.files_table_state.set(state);
}

fn render_peers(f: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app.detail.peers.iter().map(|p| {
        let dl_state = match (p.info.remote_choking, p.info.remote_interested) {
            (false, true)  => "Interested",
            (true,  _)     => "Choked",
            (false, false) => "—",
        };
        let ul_state = match (p.info.am_choking, p.info.am_interested) {
            (false, true)  => "Interested",
            (true,  _)     => "Choked",
            (false, false) => "—",
        };
        let flags = format!("{}{}{}{}",
            if p.info.extension_flags.dht               { "D" } else { " " },
            if p.info.extension_flags.extension_protocol { "H" } else { " " },
            if p.info.extension_flags.metadata           { "E" } else { " " },
            if p.info.extension_flags.pex                { "P" } else { " " },
        );
        let direction = match p.info.source {
            DirectionSnapshot::Inbound  => "Incoming",
            DirectionSnapshot::Outbound => "Outgoing",
        };
        Row::new(vec![
            Cell::from(ul_state),
            Cell::from(fmt_rate(p.info.download_rate)).style(Style::default().fg(Color::Blue)),
            Cell::from(dl_state),
            Cell::from(format!("{:.0}%", p.info.peer_progress * 100.0)),
            Cell::from("TCP"),  // protocol placeholder
            Cell::from(direction),
            Cell::from(flags),
            Cell::from(p.addr.ip().to_string()),
            Cell::from(p.addr.port().to_string()),
            Cell::from(p.info.client_name.clone()),
        ])
    }).collect();

    let widths = [
        Constraint::Length(10), // UL State
        Constraint::Length(9),  // Down
        Constraint::Length(10), // DL State
        Constraint::Length(6),  // Progress
        Constraint::Length(5),  // Connection
        Constraint::Length(9),  // Direction
        Constraint::Length(6),  // Flags
        Constraint::Length(16), // Address
        Constraint::Length(6),  // Port
        Constraint::Min(15),    // Client
    ];

    let header = Row::new(["UL State", "Down", "DL State", "Prog",
                            "Conn", "Direction", "Flags", "Address", "Port", "Client"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL)
            .title(format!(" Peers ({}) ", app.detail.peers.len())))
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let mut state = app.detail.peers_table_state.get();
    f.render_stateful_widget(table, area, &mut state);
    app.detail.peers_table_state.set(state);
}

fn render_trackers(f: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app.detail.trackers.iter().map(|t| {
        let status_style = match t.status {
            TrackerState::Ok       => Style::default().fg(Color::Green),
            TrackerState::Error    => Style::default().fg(Color::Red),
            TrackerState::Announcing => Style::default().fg(Color::Yellow),
            TrackerState::Idle     => Style::default().fg(Color::DarkGray),
        };
        Row::new(vec![
            Cell::from(t.url.clone()),
            Cell::from(format!("{:?}", t.status)).style(status_style),
            Cell::from(t.peers_received.to_string()),
            Cell::from(t.last_error.as_deref().unwrap_or("—").to_string())
                .style(Style::default().fg(Color::Red)),
        ])
    }).collect();

    let widths = [
        Constraint::Min(40),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(Row::new(["URL", "Status", "Peers", "Last Error"])
            .style(Style::default().add_modifier(Modifier::BOLD))
            .bottom_margin(1))
        .block(Block::default().borders(Borders::ALL).title(" Trackers "));

    f.render_widget(table, area);
}

fn progress_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!(
        "[{}>{}]",
        "=".repeat(filled.saturating_sub(1)),
        " ".repeat(width - filled)
    )
}

fn fmt_size(bytes: u64) -> String {
    let kb = 1024.0;
    let mb = kb * 1024.0;
    let gb = mb * 1024.0;
    let bytes = bytes as f64;
    if bytes >= gb {
        format!("{:.2} GB", bytes / gb)
    } else if bytes >= mb {
        format!("{:.2} MB", bytes / mb)
    } else if bytes >= kb {
        format!("{:.2} KB", bytes / kb)
    } else {
        format!("{} B", bytes)
    }
}

fn fmt_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1_048_576.0 {
        format!("{:>5.1}M/s", bytes_per_sec / 1_048_576.0)
    } else {
        format!("{:>5.1}K/s", bytes_per_sec / 1024.0)
    }
}

fn fmt_eta(eta: Option<u64>) -> String {
    match eta {
        None => "  --:--".to_string(),
        Some(s) if s >= 3600 => format!("{:>4}h{:02}m", s / 3600, (s % 3600) / 60),
        Some(s) => format!("{:>4}m{:02}s", s / 60, s % 60),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        format!("{:<width$}", s, width = max)
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
