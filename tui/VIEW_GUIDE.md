# Torrent Detail View — Implementation Guide

Target: press Enter on a torrent → full-screen detail view with
Overview / Files / Peers / Trackers tabs. Esc returns to list.
Bottom status bar is always visible in both views.

---

## 1. Data requirements per tab

Before writing any TUI code, establish what data each tab needs
and whether `bittorrent-core` already provides it.

### Overview tab
All fields come from `TorrentProgress` plus static metadata from
`TorrentInfo`. You need a second watch channel (or a one-shot query)
that carries static fields — info_hash, piece size, file count,
total size, save path. These don't change after metadata is fetched.

Add to `bittorrent-core`:

```rust
/// Static torrent metadata. Set once when metadata is fetched,
/// never changes after that.
#[derive(Debug, Clone)]
pub struct TorrentMeta {
    pub info_hash: InfoHash,
    pub name: String,
    pub total_bytes: u64,
    pub piece_length: u32,
    pub num_pieces: u32,
    pub num_files: usize,
    pub save_path: PathBuf,
    pub is_private: bool,
    pub comment: Option<String>,
    pub created_by: Option<String>,
}
```

Expose via `Session`:
```rust
pub async fn get_torrent_meta(
    &self,
    id: TorrentId,
) -> Result<TorrentMeta, SessionError>
```

### Files tab
File list with per-file progress. Add to `bittorrent-core`:

```rust
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub index: usize,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,  // derived from verified pieces covering this file
}
```

Expose via a `watch::Receiver<Vec<FileInfo>>` per torrent,
or a one-shot `GetFiles` query (acceptable since file list is static
structure, only progress changes):

```rust
pub async fn get_files(
    &self,
    id: TorrentId,
) -> Result<Vec<FileInfo>, SessionError>
```

### Peers tab
Covered in previous analysis. Requires widening `PeerInfo` to include
rates, bitfield progress, client name, extension flags. Then:

```rust
pub async fn get_peers(
    &self,
    id: TorrentId,
) -> Result<Vec<PeerSnapshot>, SessionError>
```

Or preferably a `watch::Receiver<Vec<PeerSnapshot>>` rebuilt on
connect/disconnect events (see efficiency discussion).

### Trackers tab
Your `TorrentEvent::TrackerAnnounced` and `TrackerError` events exist
but there is no queryable tracker state. Add:

```rust
#[derive(Debug, Clone)]
pub struct TrackerStatus {
    pub url: String,
    pub last_announce: Option<std::time::SystemTime>,
    pub next_announce: Option<std::time::SystemTime>,
    pub peers_received: u32,
    pub consecutive_errors: u32,
    pub last_error: Option<String>,
    pub status: TrackerState,
}

#[derive(Debug, Clone)]
pub enum TrackerState {
    Idle,
    Announcing,
    Ok,
    Error,
}
```

Expose via `get_trackers(id) -> Result<Vec<TrackerStatus>, SessionError>`.

---

## 2. App state changes

### View enum

Replace any ad-hoc `bool` flags with a top-level view enum:

```rust
pub enum View {
    List,
    Detail {
        id: TorrentId,
        tab: DetailTab,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub enum DetailTab {
    Overview = 0,
    Files    = 1,
    Peers    = 2,
    Trackers = 3,
}

impl DetailTab {
    fn next(self) -> Self {
        match self {
            Self::Overview => Self::Files,
            Self::Files    => Self::Peers,
            Self::Peers    => Self::Trackers,
            Self::Trackers => Self::Overview,
        }
    }
    fn prev(self) -> Self {
        match self {
            Self::Overview => Self::Trackers,
            Self::Files    => Self::Overview,
            Self::Peers    => Self::Files,
            Self::Trackers => Self::Peers,
        }
    }
    fn titles() -> Vec<&'static str> {
        vec!["Overview", "Files", "Peers", "Trackers"]
    }
}
```

### Detail cache in App

Cache the detail data so re-renders don't re-query:

```rust
pub struct DetailCache {
    pub meta: Option<TorrentMeta>,
    pub files: Vec<FileInfo>,
    pub peers: Vec<PeerSnapshot>,
    pub trackers: Vec<TrackerStatus>,
    pub files_table_state: TableState,
    pub peers_table_state: TableState,
}
```

Add to `App`:
```rust
pub view: View,
pub detail: DetailCache,
```

### Transition method on App

```rust
pub async fn open_detail(&mut self, id: TorrentId) -> anyhow::Result<()> {
    // Fetch static meta once
    let meta = self.session.get_torrent_meta(id).await?;
    self.detail = DetailCache {
        meta: Some(meta),
        files: self.session.get_files(id).await.unwrap_or_default(),
        peers: self.session.get_peers(id).await.unwrap_or_default(),
        trackers: self.session.get_trackers(id).await.unwrap_or_default(),
        files_table_state: TableState::default(),
        peers_table_state: TableState::default(),
    };
    self.view = View::Detail { id, tab: DetailTab::Overview };
    Ok(())
}

pub fn close_detail(&mut self) {
    self.view = View::List;
    self.detail = DetailCache::default();
}
```

---

## 3. Key handling changes

Add to `handle_normal_key`:

```rust
KeyCode::Enter => {
    if let Some(id) = app.selected_id() {
        app.open_detail(id).await?;
    }
}
```

Add a detail-mode key handler called when `view == View::Detail`:

```rust
fn handle_detail_key(key: KeyEvent, app: &mut App) -> bool {
    let View::Detail { ref mut tab, id } = app.view else { return false };

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.close_detail();
        }
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
            *tab = tab.next();
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
            *tab = tab.prev();
        }
        // Tab-specific navigation
        KeyCode::Down | KeyCode::Char('j') => match tab {
            DetailTab::Files    => app.detail.files_table_state.select_next(),
            DetailTab::Peers    => app.detail.peers_table_state.select_next(),
            DetailTab::Trackers => { /* tracker list scroll */ }
            DetailTab::Overview => {}
        },
        KeyCode::Up | KeyCode::Char('k') => match tab {
            DetailTab::Files    => app.detail.files_table_state.select_previous(),
            DetailTab::Peers    => app.detail.peers_table_state.select_previous(),
            _ => {}
        },
        // Refresh peers/trackers on 'r'
        KeyCode::Char('r') => {
            // re-query — fire and forget in the tick handler instead
        }
        _ => {}
    }
    false
}
```

In `handle_key`, dispatch before normal mode:

```rust
pub async fn handle_key(key: KeyEvent, app: &mut App, dialog: &mut DialogState) -> Result<bool> {
    if key.kind != KeyEventKind::Press { return Ok(false); }
    // dialogs take priority
    if !matches!(dialog, DialogState::None) {
        handle_dialog_key(key, app, dialog).await;
        return Ok(false);
    }
    // detail view
    if matches!(app.view, View::Detail { .. }) {
        return Ok(handle_detail_key(key, app));
    }
    // normal list mode
    handle_list_key(key, app, dialog).await
}
```

Refresh peers/trackers in `tick()` while detail is open:

```rust
pub async fn tick(&mut self) {
    // snapshot cache drain (always)
    for (id, rx) in &self.receivers {
        if rx.has_changed().unwrap_or(false) {
            if let Some(snap) = self.progress.get_mut(id) {
                *snap = rx.borrow_and_update().clone();
            }
        }
    }
    // refresh live detail data every N ticks
    if let View::Detail { id, tab } = self.view {
        self.detail_tick += 1;
        if self.detail_tick % 5 == 0 {  // every ~1.25s at 250ms tick
            match tab {
                DetailTab::Peers    => {
                    if let Ok(p) = self.session.get_peers(id).await {
                        self.detail.peers = p;
                    }
                }
                DetailTab::Trackers => {
                    if let Ok(t) = self.session.get_trackers(id).await {
                        self.detail.trackers = t;
                    }
                }
                _ => {}
            }
        }
    }
}
```

Note: `tick()` becoming async requires changing the select arm to
`_ = ticker.tick() => { app.tick().await; }`.

---

## 4. Layout and rendering

### Top-level ui function

```rust
fn ui(f: &mut Frame, app: &App, dialog: &DialogState) {
    let chunks = Layout::vertical([
        Constraint::Min(0),     // main content (list or detail)
        Constraint::Length(1),  // status bar — always visible
    ]).split(f.area());

    match &app.view {
        View::List => render_list_view(f, chunks[0], app, dialog),
        View::Detail { id, tab } => render_detail_view(f, chunks[0], app, *id, *tab),
    }

    render_status_bar(f, chunks[1], app);
}
```

### Status bar (always visible)

```rust
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
```

### Detail view shell

```rust
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
```

### Tab bar

```rust
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
```

### Overview tab

Split into Details block (top) + three bottom panels (State / Dates / Peers)
matching the screenshot layout:

```rust
fn render_overview(f: &mut Frame, area: Rect, app: &App, id: TorrentId) {
    let chunks = Layout::vertical([
        Constraint::Min(0),      // Details block
        Constraint::Length(7),   // bottom row
    ]).split(area);

    render_overview_details(f, chunks[0], app, id);

    let bottom = Layout::horizontal([
        Constraint::Percentage(20),   // State
        Constraint::Percentage(55),   // Dates (tewi uses more space here)
        Constraint::Percentage(25),   // Peers
    ]).split(chunks[1]);

    render_overview_state(f, bottom[0], app, id);
    render_overview_peers_summary(f, bottom[2], app, id);
    // Dates panel needs timestamps stored in TorrentMeta — add added_at field
}

fn render_overview_details(f: &mut Frame, area: Rect, app: &App, id: TorrentId) {
    let progress = app.progress.get(&id);
    let meta = app.detail.meta.as_ref();

    // Build key-value pairs as right-aligned label, value
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

// Helper: right-padded label + value on one line
fn kv_line<'a>(label: &'static str, value: &str, label_style: Style) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{:>12}: ", label), label_style),
        Span::raw(value.to_string()),
    ])
}
```

### Files tab

```rust
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
        .highlight_style(Style::default().bg(Color::DarkGray));

    // Note: app.detail.files_table_state must be &mut — pass separately or
    // use interior mutability. See architecture note below.
    f.render_widget(table, area);
    // For stateful: f.render_stateful_widget(table, area, &mut state);
}
```

### Peers tab

```rust
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
            Direction::Inbound  => "Incoming",
            Direction::Outbound => "Outgoing",
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
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    f.render_widget(table, area);
}
```

### Trackers tab

```rust
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
```

---

## 5. Architecture note: TableState mutability

`render_stateful_widget` requires `&mut TableState`. Since `ui()` takes `&App`,
you have two options:

**Option A** — pass `DetailCache` separately as `&mut`:

```rust
fn render_detail_view(
    f: &mut Frame,
    area: Rect,
    app: &App,           // immutable shared state
    cache: &mut DetailCache,  // mutable table states
    tab: DetailTab,
)
```

Split `App` so render helpers that need mutable table state take only
`&mut DetailCache`, not `&mut App`.

**Option B** — `Cell<TableState>` (interior mutability, zero cost):

```rust
pub files_table_state: std::cell::Cell<TableState>,
```

Then `ui()` can stay `&App`. Unwrap with `.get()` / `.set()`.

Option A is cleaner. Option B avoids refactoring call sites.

---

## 6. Implementation order

1. Add `TorrentMeta`, `FileInfo`, `PeerSnapshot`, `TrackerStatus` to
   `bittorrent-core` and expose query methods on `Session`.
2. Widen `PeerInfo` with rates, bitfield progress, client name, flags
   (see peer_connection.rs analysis).
3. Add `View`, `DetailTab`, `DetailCache` to the TUI app state.
4. Wire `Enter` → `open_detail`, `Esc` → `close_detail`.
5. Add `render_status_bar` — immediately visible in both views.
6. Implement `render_overview` using only `TorrentProgress` + `TorrentMeta`.
7. Implement `render_files`, `render_peers`, `render_trackers` once
   their data sources exist in `bittorrent-core`.
8. Add periodic refresh in `tick()` for Peers and Trackers tabs.
