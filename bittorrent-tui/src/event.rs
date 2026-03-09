use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use futures::{FutureExt, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{App, Mode, Tab};
use crate::torrent::AddTorrentRequest;
use crate::ui;

pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    refresh_ms: u64,
) -> Result<()> {
    let tick_rate = Duration::from_millis(refresh_ms);
    let mut event_stream = crossterm::event::EventStream::new();
    let mut tick_interval = tokio::time::interval(tick_rate);

    let (data_tx, mut data_rx) = tokio::sync::mpsc::channel(1);
    let mut is_fetching = false;

    // Initial refresh
    app.refresh().await?;

    loop {
        terminal.draw(|f| ui::render(f, app))?;

        tokio::select! {
            _ = tick_interval.tick() => {
                if !is_fetching {
                    is_fetching = true;
                    let c = app.client.clone();
                    let tx = data_tx.clone();
                    let selected_hex = app.list_state.selected()
                        .and_then(|i| app.torrents.get(i))
                        .map(|s| s.id.to_hex());

                    tokio::spawn(async move {
                        let torrents = c.get_torrents().await.unwrap_or_default();
                        let stats = c.get_stats().await.unwrap_or_default();
                        let selected = if let Some(hex) = selected_hex {
                            c.get_torrent_details(&hex).await.ok()
                        } else {
                            None
                        };
                        let _ = tx.send((torrents, stats, selected)).await;
                    });
                }
            }
            Some((torrents, stats, selected)) = data_rx.recv() => {
                is_fetching = false;
                app.torrents = torrents;
                app.stats = stats;
                app.selected_torrent = selected;

                app.download_history.push_back(app.stats.total_download_rate);
                if app.download_history.len() > 60 {
                    app.download_history.pop_front();
                }
                app.upload_history.push_back(app.stats.total_upload_rate);
                if app.upload_history.len() > 60 {
                    app.upload_history.pop_front();
                }
            }
            maybe_event = event_stream.next().fuse() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind == KeyEventKind::Press {
                            match app.mode {
                                Mode::Main => match key.code {
                                    KeyCode::Char('q') => app.should_quit = true,
                                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                                    KeyCode::Char('g') => app.first(),
                                    KeyCode::Char('G') => app.last(),
                                    KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => app.active_tab = Tab::Details,
                                    KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => app.active_tab = Tab::Torrents,
                                    KeyCode::Tab => {
                                        if app.active_tab == Tab::Details {
                                            app.details_tab = (app.details_tab + 1) % 3;
                                        }
                                    }
                                    KeyCode::Char('a') => {
                                        app.mode = Mode::AddMagnet;
                                        app.input.clear();
                                    }
                                    KeyCode::Char('f') => {
                                        app.mode = Mode::FileBrowser;
                                    }
                                    KeyCode::Char('x') => {
                                        if let Some(index) = app.list_state.selected() &&
                                            let Some(t) = app.torrents.get(index) {
                                                let url = format!("{}/torrents/{}", app.client.base_url, t.id.to_hex());
                                                let client = app.client.client.clone();
                                                tokio::spawn(async move {
                                                    let _ = client.delete(url).send().await;
                                                });
                                        }
                                    }
                                    _ => {}
                                },
                                Mode::AddMagnet => match key.code {
                                    KeyCode::Enter => {
                                        let uri = app.input.clone();
                                        let client = app.client.client.clone();
                                        let base_url = app.client.base_url.clone();
                                        tokio::spawn(async move {
                                            let url = format!("{}/torrents", base_url);
                                            let req = AddTorrentRequest { path: None, uri: Some(uri) };
                                            let _ = client.post(url).json(&req).send().await;
                                        });
                                        app.mode = Mode::Main;
                                        app.set_status("Adding magnet URI...");
                                    }
                                    KeyCode::Char(c) => {
                                        app.input.push(c);
                                    }
                                    KeyCode::Backspace => {
                                        app.input.pop();
                                    }
                                    KeyCode::Esc => {
                                        app.mode = Mode::Main;
                                    }
                                    _ => {}
                                },
                                Mode::FileBrowser => match key.code {
                                    KeyCode::Char('j') | KeyCode::Down => app.file_browser.next(),
                                    KeyCode::Char('k') | KeyCode::Up => app.file_browser.previous(),
                                    KeyCode::Enter | KeyCode::Right => {
                                        if let Some(path) = app.file_browser.enter() {
                                            let path_str = path.to_string_lossy().to_string();
                                            let client = app.client.client.clone();
                                            let base_url = app.client.base_url.clone();
                                            tokio::spawn(async move {
                                                let url = format!("{}/torrents", base_url);
                                                let req = AddTorrentRequest { path: Some(path_str), uri: None };
                                                let _ = client.post(url).json(&req).send().await;
                                            });
                                            app.mode = Mode::Main;
                                            app.set_status("Adding torrent file...");
                                        }
                                    }
                                    KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                                        app.file_browser.parent();
                                    }
                                    KeyCode::Esc | KeyCode::Char('q') => {
                                        app.mode = Mode::Main;
                                    }
                                    _ => {}
                                },
                            }
                        }
                    }
                    Some(Ok(_)) => {} // Ignore other events
                    Some(Err(_)) | None => {
                        break;
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
