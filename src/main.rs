use std::{
    env,
    error::Error,
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
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
    list_state: ListState,
    input_mode: InputMode,
    input_buffer: String,
    client: Client,
    last_refresh: Instant,
}

#[derive(PartialEq, Eq, Clone)]
enum InputMode {
    Normal,
    AddingDownload,
}

impl App {
    fn new(downloader_url: String) -> Self {
        let mut list_state = ListState::default();
        list_state.select(None); // Initially no selection
        
        App {
            downloader_url,
            downloads: Vec::new(),
            list_state,
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
            let prev_selected = self.list_state.selected();
            self.downloads = response.json().await?;
            self.last_refresh = Instant::now();
            
            // Preserve selection if possible
            if !self.downloads.is_empty() {
                if let Some(selected) = prev_selected {
                    self.list_state.select(Some(selected.min(self.downloads.len() - 1)));
                }
            } else {
                self.list_state.select(None);
            }
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

    async fn control_download(&self, model_name: &str, action: &str) -> Result<(), Box<dyn Error>> {
        let control_url = format!("{}/downloads/{}/{}", self.downloader_url, model_name, action);
        let response = self.client.post(&control_url).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("Failed to {} download: {}", action, response.status()).into())
        }
    }

    fn select_next(&mut self) {
        if self.downloads.is_empty() {
            self.list_state.select(None);
            return;
        }

        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.downloads.len() - 1 {
                    i
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn select_previous(&mut self) {
        if self.downloads.is_empty() {
            self.list_state.select(None);
            return;
        }

        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    0
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn selected_model_name(&self) -> Option<&str> {
        self.list_state
            .selected()
            .map(|i| self.downloads[i].modelName.as_str())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let downloader_url = env::args()
        .nth(1)
        .unwrap_or_else(|| {
            env::var("DOWNLOADER_URL").unwrap_or_else(|_| "http://localhost:8080".to_string())
        });

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = Arc::new(Mutex::new(App::new(downloader_url)));

    // Background refresh task
    let app_clone = app.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            let mut app = app_clone.lock().await;
            if let Err(e) = app.fetch_downloads().await {
                eprintln!("Error fetching downloads: {}", e);
            }
        }
    });

    let res = run_app(&mut terminal, app).await;

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}

async fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: Arc<Mutex<App>>,
) -> Result<(), Box<dyn Error>> {
    loop {
        {
            let mut app = app.lock().await;
            terminal.draw(|f| ui(f, &mut app))?;
        }

        if let Event::Key(key) = event::read()? {
            let mut app = app.lock().await;

            match app.input_mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('a') => {
                        app.input_mode = InputMode::AddingDownload;
                    }
                    KeyCode::Char('s') => {
                        if let Some(model_name) = app.selected_model_name() {
                            let model_name = model_name.to_string();
                            if let Err(e) = app.control_download(&model_name, "stop").await {
                                eprintln!("Error stopping download: {}", e);
                            }
                            app.fetch_downloads().await?;
                        }
                    }
                    KeyCode::Char('r') => {
                        if let Some(model_name) = app.selected_model_name() {
                            let model_name = model_name.to_string();
                            if let Err(e) = app.control_download(&model_name, "restart").await {
                                eprintln!("Error restarting download: {}", e);
                            }
                            app.fetch_downloads().await?;
                        }
                    }
                    KeyCode::Char('p') => {
                        if let Some(model_name) = app.selected_model_name() {
                            let model_name = model_name.to_string();
                            if let Err(e) = app.control_download(&model_name, "pause").await {
                                eprintln!("Error pausing download: {}", e);
                            }
                            app.fetch_downloads().await?;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                    KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
                    _ => {}
                },
                InputMode::AddingDownload => match key.code {
                    KeyCode::Enter => {
                        let url = app.input_buffer.clone();
                        app.input_buffer.clear();
                        app.input_mode = InputMode::Normal;
                        if !url.is_empty() {
                            app.add_download(url).await?;
                        }
                    }
                    KeyCode::Char(c) => app.input_buffer.push(c),
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

fn ui<B: Backend>(f: &mut Frame<B>, app: &mut App) {
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

            ListItem::new(vec![Spans::from(vec![
                Span::styled(
                    format!("{} ", download.modelName),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    "Status: {}, Last Change: {}",
                    download.status, time_str
                )),
            ])])
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Downloads"))
        .highlight_style(
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );

    // Calculate the [A/B] string
    let selection_index = app.list_state.selected().map(|i| i + 1).unwrap_or(0);
    let total_items = app.downloads.len();
    let position_string = format!("[{}/{}]", selection_index, total_items);

    // Create a span for the position string
    let position_span = Span::styled(
        position_string,
        Style::default().fg(Color::Gray),
    );

    // Add the span to the title
    let title = Spans::from(vec![
        Span::raw("Downloads "),
        position_span,
    ]);

    // Update the list block with the new title
    let list = list.block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
    );

    f.render_stateful_widget(list, chunks[0], &mut app.list_state);

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
        let input_rect = Rect::new(chunks[0].x + 1, chunks[0].y + 1, chunks[0].width - 2, 3);
        f.render_widget(
            Paragraph::new(app.input_buffer.as_ref())
                .block(Block::default().borders(Borders::ALL).title("Enter URL")),
            input_rect,
        );
    }
}
