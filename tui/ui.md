# TUI Refactor Prompt

You are refactoring a Ratatui-based BitTorrent TUI client in Rust.
The codebase has three files to modify: `main.rs`, `app.rs`, `command.rs`.
The backing library is `bittorrent-core` whose `Session` exposes:
- `add_torrent(path)` -> `Result<TorrentId, SessionError>`
- `add_magnet(uri)` -> `Result<TorrentId, SessionError>`
- `remove_torrent(id)` -> `Result<(), SessionError>`
- `shutdown()` -> `Result<(), SessionError>`
- `subscribe_torrent(id)` -> `Result<watch::Receiver<TorrentProgress>, SessionError>`
- `subscribe()` -> `broadcast::Receiver<SessionEvent>`

`TorrentProgress` has fields:
```rust
pub name: String,
pub total_pieces: u32,
pub verified_pieces: u32,
pub failed_pieces: u32,
pub total_bytes: u64,
pub downloaded_bytes: u64,
pub uploaded_bytes: u64,
pub connected_peers: u32,
pub download_rate: f64,   // bytes/sec
pub upload_rate: f64,
pub state: TorrentState,
pub eta_seconds: Option<u64>,
```

`TorrentState` variants: `FetchingMetadata`, `Checking`, `Downloading`,
`Seeding`, `Paused`, `Error(String)`, `Finished`.

`SessionEvent` variants that matter: `TorrentAdded(TorrentId)`,
`TorrentRemoved(TorrentId)`, `TorrentCompleted(TorrentId)`.

---

## Task 1 — `command.rs`

Define these message structs (no channels, no enums — plain structs):

```rust
pub struct AddTorrentCommand { pub value: String }
pub struct RemoveTorrentCommand { pub id: TorrentId }
pub struct OpenAddTorrentCommand;
pub struct OpenRemoveTorrentCommand { pub id: TorrentId }
```

---

## Task 2 — `app.rs`

Replace the existing `App` with:

```rust
pub struct App {
    pub session: Session,
    pub torrents: HashMap<TorrentId, watch::Receiver<TorrentProgress>>,
    pub torrent_order: Vec<TorrentId>,   // insertion-ordered for stable rendering
    pub selected_index: usize,
    pub should_quit: bool,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
}
```

Implement these methods:

- `new(session: Session) -> Self`
- `tick(&mut self)` — no-op for now
- `quit(&mut self)` — sets `should_quit = true`
- `async add_torrent_to_state(&mut self, id: TorrentId) -> anyhow::Result<()>`
  — subscribes via `session.subscribe_torrent(id)`, inserts into both
  `torrents` and `torrent_order` (skip if already present)
- `remove_torrent_from_state(&mut self, id: TorrentId)`
  — removes from `torrents` and `torrent_order`, clamps `selected_index`
- `selected_id(&self) -> Option<TorrentId>`
  — returns `torrent_order.get(selected_index).copied()`
- `set_error(&mut self, e: impl Display)` — sets `last_error`
- `set_status(&mut self, msg: impl Into<String>)` — sets `status_message`,
  clears `last_error`
- `clear_status(&mut self)` — clears both fields

---

## Task 3 — `main.rs`

### Dialog state

Introduce a `DialogState` enum at the top of the file:

```rust
enum DialogState {
    None,
    AddTorrent(String),       // current input buffer
    ConfirmRemove(TorrentId),
}
```

### `run_app` must only contain:

1. Setup of `EventStream`, `session_events`, `ticker`, `dialog: DialogState`
2. The `loop { select! { ... } }` block
3. Each `select!` arm calls a dedicated handler function — no logic inline

The four handler functions to extract:

```rust
async fn handle_key_event(
    key: KeyEvent,
    app: &mut App,
    dialog: &mut DialogState,
) -> Result<bool>   // returns true = quit
```

```rust
async fn handle_session_event(
    event: SessionEvent,
    app: &mut App,
)
```

```rust
fn handle_tick(app: &mut App)
```

```rust
fn handle_add_dialog_key(
    key: KeyEvent,
    input: &mut String,
    app: &mut App,
    dialog: &mut DialogState,
)
```

Inside `handle_key_event`:
- If dialog is `AddTorrent`, delegate to `handle_add_dialog_key`
- If dialog is `ConfirmRemove(id)`:
  - `y` or `Enter` → call `app.session.remove_torrent(id).await`, then
    `app.remove_torrent_from_state(id)` on success, `app.set_error(e)` on error,
    then set `dialog = DialogState::None`
  - any other key → set `dialog = DialogState::None`
- Normal mode bindings:
  - `q` → shutdown and return `true`
  - `a` → set `dialog = DialogState::AddTorrent(String::new())`
  - `d` → if `app.selected_id()` is Some, set `dialog = DialogState::ConfirmRemove(id)`
  - `j` / `Down` → increment selected_index mod len
  - `k` / `Up` → decrement selected_index mod len

Inside `handle_add_dialog_key`:
- `Esc` → `dialog = DialogState::None`
- `Backspace` → pop from input
- `Char(c)` → push to input
- `Enter` → submit: detect magnet by `value.starts_with("magnet:")`,
  call appropriate session method, set status or error on app,
  set `dialog = DialogState::None`

### `ui` function signature:

```rust
fn ui(f: &mut Frame, app: &App, dialog: &DialogState)
```

Split into sub-functions:

```rust
fn render_header(f: &mut Frame, area: Rect, app: &App)
fn render_torrent_list(f: &mut Frame, area: Rect, app: &App)
fn render_footer(f: &mut Frame, area: Rect)
fn render_dialog(f: &mut Frame, area: Rect, app: &App, dialog: &DialogState)
```

Layout: 4 vertical chunks with constraints:
`[Length(3), Min(0), Length(3), Length(3)]`

---

## Task 4 — Torrent list rows

Each row is a `Line` composed of multiple `Span`s with styles.
Do NOT use a plain format string. Use `ratatui::text::{Line, Span}` and
`ratatui::style::{Color, Modifier, Style}`.

**Row layout** (all on one line, fixed-width columns):

```
[STATE_BADGE] NAME (truncated to 36 chars)  PROGRESS_BAR  dl:X.X K/s  ul:X.X K/s  peers:N  ETA
```

**State badge** — colored background, 2-char symbol:

| State            | Symbol | bg Color          |
|------------------|--------|-------------------|
| FetchingMetadata | `MT`   | `Color::DarkGray` |
| Checking         | `CK`   | `Color::Yellow`   |
| Downloading      | `DL`   | `Color::Blue`     |
| Seeding          | `SD`   | `Color::Green`    |
| Paused           | `PA`   | `Color::Gray`     |
| Error(_)         | `ER`   | `Color::Red`      |
| Finished         | `FN`   | `Color::Cyan`     |

Badge span: `" XX "` with `Style::default().bg(color).fg(Color::Black).add_modifier(Modifier::BOLD)`

**Progress bar** — 12 chars wide, ASCII block:

```rust
fn progress_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("[{}>{}]",
        "=".repeat(filled.saturating_sub(1)),
        " ".repeat(width - filled),
    )
}
```

When `total_bytes == 0` (metadata not yet fetched): render `[????????????]`.

**Rate formatting** helper:

```rust
fn fmt_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1_048_576.0 {
        format!("{:>5.1}M/s", bytes_per_sec / 1_048_576.0)
    } else {
        format!("{:>5.1}K/s", bytes_per_sec / 1024.0)
    }
}
```

**ETA formatting** helper:

```rust
fn fmt_eta(eta: Option<u64>) -> String {
    match eta {
        None => "  --:--".to_string(),
        Some(s) if s >= 3600 => format!("{:>4}h{:02}m", s / 3600, (s % 3600) / 60),
        Some(s) => format!("{:>4}m{:02}s", s / 60, s % 60),
    }
}
```

**Name truncation** helper:

```rust
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        format!("{:<width$}", s, width = max)
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
```

**Full row assembly** — extract to:

```rust
fn torrent_row(progress: &TorrentProgress) -> Line<'static>
```

Assemble as a `Vec<Span>` then `Line::from(spans)`.

Apply dim style to rate/eta spans when the torrent is not in
`Downloading` or `Seeding` state.

---

## Task 5 — `render_dialog`

Three cases:

1. `DialogState::AddTorrent(input)` — yellow border, title `"Add Torrent"`,
   text `format!("Path or magnet URI: {}_", input)` (the `_` acts as cursor)

2. `DialogState::ConfirmRemove(id)` — red border, title `"Confirm Remove"`,
   show the torrent name from `app.torrents[id].borrow().name` in the text,
   text: `format!("Remove \"{}\"? [y] yes  [Esc] cancel", name)`

3. `DialogState::None` — if `app.last_error` is Some, show it with red fg;
   else if `app.status_message` is Some, show it with green fg;
   else render an empty block (no text, just the border)

---

## Constraints

- No `unwrap()` in non-test code — use `?` or match.
- `ui` and all render sub-functions take `&App` not `&mut App`.
- `torrent_order` is the single source of truth for list ordering —
  never iterate `app.torrents` directly in render code.
- Do not add any dependencies beyond what is already in `Cargo.toml`
  (`ratatui`, `crossterm`, `tokio`, `anyhow`, `futures-util`,
  `bittorrent-core`).
- Keep `run_app` under 40 lines.
