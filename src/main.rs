use std::{
    env,
    error::Error,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum DownloadStatus {
    Downloading,
    Initializing,
    Retrying,
    RetryingWithMessage(String),
    Offline,
    Paused,
    PausedForExclusiveShow,
    PausedForTicketShow,
    Error,
    ErrorWithMessage(String),
    Completed,
}

impl std::fmt::Display for DownloadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            DownloadStatus::Downloading => write!(f, "Downloading"),
            DownloadStatus::Initializing => write!(f, "Initializing"),
            DownloadStatus::Retrying => write!(f, "Retrying"),
            DownloadStatus::RetryingWithMessage(msg) => write!(f, "Retrying: {}", msg),
            DownloadStatus::Offline => write!(f, "Offline"),
            DownloadStatus::Paused => write!(f, "Paused"),
            DownloadStatus::PausedForExclusiveShow => write!(f, "Paused for Exclusive Show"),
            DownloadStatus::PausedForTicketShow => write!(f, "Paused for Ticket Show"),
            DownloadStatus::Error => write!(f, "Error"),
            DownloadStatus::ErrorWithMessage(msg) => write!(f, "Error: {}", msg),
            DownloadStatus::Completed => write!(f, "Completed"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Download {
    modelName: String,
    status: DownloadStatus,
    startTime: DateTime<Utc>,
    lastStatusChange: DateTime<Utc>,
    retryCount: u32,
}

struct App {
    downloader_url: String,
    downloads: Vec<Download>,
    selected_download: Option<usize>,
    input_mode: InputMode,
    input_buffer: String,
    client: Client,
    last_refresh: Instant,
}

#[derive(PartialEq, Eq)]
enum InputMode {
    Normal,
    AddingDownload,
}

impl App {
    async fn new(downloader_url: String) -> Self {
        App {
            downloader_url,
            downloads: Vec::new(),
            selected_download: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            client: Client::new(),
            last_refresh: Instant::now(),
        }
    }

    async fn fetch_downloads(&mut self) -> Result<(), Box<dyn Error>> {
        let url = format!("{}/downloads", self.downloader_url);
        let response = self.client.get(&url).send().await?;

        if response.status().is_success() {
            self.downloads = response.json().await?;
            self.last_refresh = Instant::now();
            Ok(())
        } else {
            Err(format!("Failed to fetch downloads: {}", response.status()).into())
        }
    }

    async fn add_download(&mut self, url: String) -> Result<(), Box<dyn Error>> {
        let add_url = format!("{}/downloads", self.downloader_url);
        let response = self
            .client
            .post(&add_url)
            .json(&serde_json::json!({"url": url}))
            .send()
            .await?;

        if response.status().is_success() {
            self.fetch_downloads().await?;
            Ok(())
        } else {
            Err(format!("Failed to add download: {}", response.status()).into())
        }
    }

    async fn stop_download(&self, model_name: &str) -> Result<(), Box<dyn Error>> {
        self.control_download(model_name, "stop").await
    }

    async fn restart_download(&self, model_name: &str) -> Result<(), Box<dyn Error>> {
        self.control_download(model_name, "restart").await
    }

    async fn pause_download(&self, model_name: &str) -> Result<(), Box<dyn Error>> {
        self.control_download(model_name, "pause").await
    }

    async fn control_download(&self, model_name: &str, action: &str) -> Result<(), Box<dyn Error>> {
        let control_url = format!("{}/{}/{}", self.downloader_url, model_name, action);
        let response = self.client.post(&control_url).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("Failed to {} download: {}", action, response.status()).into())
        }
    }

    fn select_next(&mut self) {
        if self.downloads.is_empty() {
            self.selected_download = None;
            return;
        }

        self.selected_download = match self.selected_download {
            None => Some(0),
            Some(i) => Some(std::cmp::min(i + 1, self.downloads.len() - 1)),
        };
    }

    fn select_previous(&mut self) {
        if self.downloads.is_empty() {
            self.selected_download = None;
            return;
        }

        self.selected_download = match self.selected_download {
            None => Some(0),
            Some(i) => Some(std::cmp::max(i as i32 - 1, 0) as usize),
        };
    }

    fn selected_model_name(&self) -> Option<String> {
        self.selected_download.map(|i| self.downloads[i].modelName.clone())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let downloader_url = match env::args().nth(1) {
        Some(url) => url,
        None => env::var("DOWNLOADER_URL").map_err(|_| "DOWNLOADER_URL not set")?,
    };

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = Arc::new(Mutex::new(App::new(downloader_url).await));

    // run the main loop in a separate thread
    let app_clone = app.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            let mut app = app_clone.lock().unwrap();
            if let Err(e) = app.fetch_downloads().await {
                eprintln!("Error fetching downloads: {}", e);
            }
        }
    });

    let res = run_app(&mut terminal, app);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: Arc<Mutex<App>>,
) -> Result<(), Box<dyn Error>> {
    loop {
        terminal.draw(|f| ui(f, app.clone()))?;

        if let Event::Key(key) = event::read()? {
            let mut app = app.lock().unwrap();
            match app.input_mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('a') => {
                        app.input_mode = InputMode::AddingDownload;
                    }
                    KeyCode::Char('s') => {
                        if let Some(model_name) = app.selected_model_name() {
                            let model_name = model_name.clone();
                            let app_clone = app.clone();
                            tokio::spawn(async move {
                                if let Err(e) = app_clone.stop_download(&model_name).await {
                                    eprintln!("Error stopping download: {}", e);
                                }
                                let mut app = app_clone.lock().unwrap();
                                app.fetch_downloads().await.unwrap();
                            });
                        }
                    }
                    KeyCode::Char('r') => {
                        if let Some(model_name) = app.selected_model_name() {
                            let model_name = model_name.clone();
                            let app_clone = app.clone();
                            tokio::spawn(async move {
                                if let Err(e) = app_clone.restart_download(&model_name).await {
                                    eprintln!("Error restarting download: {}", e);
                                }
                                let mut app = app_clone.lock().unwrap();
                                app.fetch_downloads().await.unwrap();
                            });
                        }
                    }
                    KeyCode::Char('p') => {
                        if let Some(model_name) = app.selected_model_name() {
                            let model_name = model_name.clone();
                            let app_clone = app.clone();
                            tokio::spawn(async move {
                                if let Err(e) = app_clone.pause_download(&model_name).await {
                                    eprintln!("Error pausing download: {}", e);
                                }
                                let mut app = app_clone.lock().unwrap();
                                app.fetch_downloads().await.unwrap();
                            });
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                    KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
                    _ => {}
                },
                InputMode::AddingDownload => match key.code {
                    KeyCode::Enter => {
                        let url = app.input_buffer.drain(..).collect();
                        let app_clone = app.clone();
                        tokio::spawn(async move {
                            if let Err(e) = app_clone.add_download(url).await {
                                eprintln!("Error adding download: {}", e);
                            }
                            let mut app = app_clone.lock().unwrap();
                            app.fetch_downloads().await.unwrap();
                        });
                        app.input_mode = InputMode::Normal;
                    }
                    KeyCode::Char(c) => {
                        app.input_buffer.push(c);
                    }
                    KeyCode::Backspace => {
                        app.input_buffer.pop();
                    }
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.input_buffer.clear();
                    }
                    _ => {}
                },
            }
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app_mutex: Arc<Mutex<App>>) {
    let app = app_mutex.lock().unwrap();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)].as_ref())
        .split(f.size());

    let items: Vec<ListItem> = app
        .downloads
        .iter()
        .map(|download| {
            let time_since_last_change = Utc::now() - download.lastStatusChange;
            let time_str = if time_since_last_change.num_seconds() < 60 {
                format!("{}s", time_since_last_change.num_seconds())
            } else {
                format!("{}m", time_since_last_change.num_minutes())
            };

            let style = if let Some(selected) = app.selected_download {
                if app.downloads[selected].modelName == download.modelName {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }
            } else {
                Style::default()
            };

            ListItem::new(vec![
                Spans::from(vec![
                    Span::styled(
                        format!("{} ", download.modelName),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("Status: {}, Last Change: {}", download.status, time_str)),
                ]),
            ])
            .style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Downloads"));
    f.render_widget(list, chunks[0]);

    let shortcuts = Paragraph::new(Text::from(Spans::from(vec![
        Span::raw("[A]dd Download "),
        Span::raw("[S]top Download "),
        Span::raw("[R]estart Download "),
        Span::raw("[P]ause Download "),
        Span::raw("[Q]uit"),
    ])))
    .block(Block::default().borders(Borders::ALL).title("Shortcuts"));

    f.render_widget(shortcuts, chunks[1]);

    if app.input_mode == InputMode::AddingDownload {
        let input_rect = Rect::new(
            chunks[0].x + 1,
            chunks[0].y + 1,
            chunks[0].width - 2,
            3,
        );
        f.render_widget(
            Paragraph::new(app.input_buffer.as_ref())
                .block(Block::default().borders(Borders::ALL).title("Enter URL")),
            input_rect,
        );
    }
}
