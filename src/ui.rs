//! Keyboard-first terminal frontend. The runtime never owns the terminal.
use crate::{
    accounting::usd,
    model::{Event, EventKind, Role},
    runtime::{Agent, Frontend},
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
};
use futures_util::StreamExt;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use unicode_width::UnicodeWidthStr;

pub enum UiMessage {
    Event(Event),
    Approval {
        title: String,
        preview: String,
        reply: oneshot::Sender<Option<bool>>,
    },
    Done,
}
struct Channel {
    tx: mpsc::Sender<UiMessage>,
}
pub fn channel() -> (Arc<dyn Frontend>, mpsc::Receiver<UiMessage>) {
    let (tx, rx) = mpsc::channel(128);
    (Arc::new(Channel { tx }), rx)
}
#[async_trait]
impl Frontend for Channel {
    async fn event(&self, event: Event) -> Result<()> {
        self.tx
            .send(UiMessage::Event(event))
            .await
            .context("terminal closed")
    }
    async fn approve(&self, _id: &str, title: &str, preview: &str) -> Result<Option<bool>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(UiMessage::Approval {
                title: title.into(),
                preview: preview.into(),
                reply: tx,
            })
            .await
            .context("terminal closed")?;
        rx.await.context("approval dialog closed")
    }
}
struct Pending {
    title: String,
    preview: String,
    reply: oneshot::Sender<Option<bool>>,
    scroll: u16,
}
struct View {
    transcript: String,
    input: String,
    cursor: usize,
    activity: Vec<String>,
    usage: String,
    busy: bool,
    panels: bool,
    palette: bool,
    selection: usize,
    dismiss_slash: bool,
    pending: Option<Pending>,
    scroll: u16,
    status: String,
    session: String,
    model: String,
    workspace: String,
}
impl View {
    fn new(agent: &Agent) -> Self {
        Self {
            transcript: String::new(),
            input: String::new(),
            cursor: 0,
            activity: vec![],
            usage: "No requests yet · unpriced until reported".into(),
            busy: false,
            panels: true,
            palette: false,
            selection: 0,
            dismiss_slash: false,
            pending: None,
            scroll: 0,
            status: "Ready".into(),
            session: agent.session.clone(),
            model: agent.model.model.clone(),
            workspace: agent.tools.root().display().to_string(),
        }
    }
    fn event(&mut self, event: Event) {
        match event.kind {
            EventKind::Started => {
                self.busy = true;
                self.status = "Thinking… · Esc cancels".into();
                self.transcript.push_str("\nFORGE\n");
            }
            EventKind::Text { text } => {
                self.transcript.push_str(&text);
            }
            EventKind::Activity { text } => {
                self.activity.push(text.clone());
                self.status = text;
            }
            EventKind::Usage {
                reported,
                reserved_nusd,
                cost_nusd,
                warning,
                ..
            } => {
                self.usage = match reported {
                    Some(u) => format!(
                        "{} input\n{} output\n{} cached\n{} reasoning\n\nCost: {}\nReserved: {}",
                        u.input,
                        u.output,
                        u.cached_input,
                        u.reasoning,
                        usd(cost_nusd),
                        usd(reserved_nusd)
                    ),
                    None => format!(
                        "Request reserved\n{}\nAwaiting reported usage",
                        usd(reserved_nusd)
                    ),
                };
                if warning {
                    self.usage.push_str("\n\nBudget ≥80%");
                }
            }
            EventKind::Error { message } => {
                self.transcript.push_str(&format!("\n\nError: {message}\n"));
                self.status = "Request failed · inspect usage before retrying".into();
            }
            EventKind::Finished { status } => {
                self.busy = false;
                self.status = status;
                self.transcript.push('\n');
            }
            EventKind::Compacted => self
                .activity
                .push("Earlier context summarized; full history retained".into()),
            EventKind::ApprovalRequired { .. } => {}
        }
    }
    fn insert(&mut self, text: &str) {
        self.selection = 0;
        self.dismiss_slash = false;
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }
    fn left(&mut self) {
        self.cursor = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
    fn right(&mut self) {
        self.cursor += self.input[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
    }
    fn backspace(&mut self) {
        self.selection = 0;
        self.dismiss_slash = false;
        let end = self.cursor;
        self.left();
        self.input.drain(self.cursor..end);
    }
}

#[derive(Clone, Copy)]
enum Command {
    Panels,
    Usage,
    Cancel,
    Quit,
    Update,
}

const COMMANDS: [(Command, &str, &str); 5] = [
    (
        Command::Panels,
        "/panels",
        "Toggle activity and usage panels",
    ),
    (Command::Usage, "/usage", "Show latest request usage"),
    (Command::Cancel, "/cancel", "Cancel the current turn"),
    (Command::Quit, "/quit", "Exit Forge when idle"),
    (Command::Update, "/update", "Fetch and update Forge source"),
];

enum MenuAction {
    Handled,
    Quit,
    Update,
}

impl View {
    fn slash_query(&self) -> Option<&str> {
        let query = self.input.strip_prefix('/')?;
        (!query.contains(char::is_whitespace)).then_some(query)
    }

    fn menu_open(&self) -> bool {
        self.palette || (!self.dismiss_slash && self.slash_query().is_some())
    }

    fn commands(&self) -> Vec<(Command, &'static str, &'static str)> {
        let query = if self.palette {
            ""
        } else {
            self.slash_query().unwrap_or("")
        };
        COMMANDS
            .iter()
            .copied()
            .filter(|(_, name, _)| name[1..].starts_with(query))
            .collect()
    }

    fn menu_key(&mut self, key: KeyEvent, cancel: &CancellationToken) -> Option<MenuAction> {
        if !self.menu_open() {
            return None;
        }
        let commands = self.commands();
        match key.code {
            KeyCode::Esc => {
                self.palette = false;
                self.dismiss_slash = true;
            }
            KeyCode::Up => self.selection = self.selection.saturating_sub(1),
            KeyCode::Down => {
                self.selection = (self.selection + 1).min(commands.len().saturating_sub(1))
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let Some(&(command, _, _)) = commands.get(self.selection) else {
                    return Some(MenuAction::Handled);
                };
                if !self.palette {
                    self.input.clear();
                    self.cursor = 0;
                }
                self.palette = false;
                self.dismiss_slash = true;
                match command {
                    Command::Panels => {
                        self.panels = !self.panels;
                        self.status = if self.panels {
                            "Panels enabled"
                        } else {
                            "Panels hidden"
                        }
                        .into();
                    }
                    Command::Usage => {
                        self.panels = true;
                        self.status = self
                            .usage
                            .lines()
                            .filter(|line| !line.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ");
                    }
                    Command::Cancel => {
                        if self.busy {
                            cancel.cancel();
                        }
                    }
                    Command::Quit if !self.busy => return Some(MenuAction::Quit),
                    Command::Quit => self.status = "Cancel the current turn before quitting".into(),
                    Command::Update if !self.busy => return Some(MenuAction::Update),
                    Command::Update => {
                        self.status = "Wait for the current turn before updating".into()
                    }
                }
            }
            _ if self.palette => {}
            _ => return None,
        }
        Some(MenuAction::Handled)
    }
}

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

pub async fn run(mut agent: Agent, mut rx: mpsc::Receiver<UiMessage>) -> Result<()> {
    let mut view = View::new(&agent);
    for m in agent.store.context(&agent.session)? {
        if matches!(m.role, Role::User | Role::Assistant) {
            view.transcript.push_str(&format!(
                "\n{}\n{}\n",
                if m.role == Role::User { "YOU" } else { "FORGE" },
                m.text
            ));
        }
    }
    let mut terminal = ratatui::try_init()?;
    let _restore = Restore;
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
    let mut task: Option<tokio::task::JoinHandle<()>> = None;
    let mut update: Option<tokio::task::JoinHandle<Result<String>>> = None;
    loop {
        terminal.draw(|f| draw(f, &view))?;
        tokio::select! {
            _=tick.tick()=>{
                if task.as_ref().is_some_and(|t|t.is_finished()){if let Some(t)=task.take(){let _=t.await;}view.busy=false;}
                if update.as_ref().is_some_and(|job| job.is_finished()) && let Some(job) = update.take() {
                    let status = match job.await {
                        Ok(Ok(message)) => message,
                        Ok(Err(error)) => format!("Update failed: {error:#}"),
                        Err(error) => format!("Update stopped: {error}"),
                    };
                    view.status = status;
                }
            }
            Some(message)=rx.recv()=>match message{
                UiMessage::Event(event)=>view.event(event),
                UiMessage::Approval{title,preview,reply}=>{view.pending=Some(Pending{title,preview,reply,scroll:0});view.status="Approval required · y approve · n deny".into();}
                UiMessage::Done=>view.busy=false,
            },
            event=events.next()=>{
                let Some(event)=event else{break;};
                match event? {
                    TermEvent::Paste(text)=>if view.pending.is_none()&&!view.palette{view.insert(&text.replace('\r',"").replace('\t',"    "));},
                    TermEvent::Key(key) if matches!(key.kind,KeyEventKind::Press|KeyEventKind::Repeat)=>{
                        if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('c') {
                            if update.is_some() {
                                view.status = "Wait for the Forge update to finish before quitting".into();
                                continue;
                            }
                            if view.busy {agent.cancel.cancel();if let Some(p)=view.pending.take(){let _=p.reply.send(Some(false));}view.status="Cancelling…".into();}
                            else{break;}continue;
                        }
                        if let Some(p)=view.pending.as_mut(){
                            match key.code{
                                KeyCode::Char('y')=>{if let Some(p)=view.pending.take(){let _=p.reply.send(Some(true));}}
                                KeyCode::Char('n')|KeyCode::Esc=>{if let Some(p)=view.pending.take(){let _=p.reply.send(Some(false));}}
                                KeyCode::PageDown|KeyCode::Down=>p.scroll=p.scroll.saturating_add(5),
                                KeyCode::PageUp|KeyCode::Up=>p.scroll=p.scroll.saturating_sub(5),
                                _=>{},
                            }continue;
                        }
                        if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('p'){
                            view.palette = !view.palette;
                            view.selection = 0;
                            continue;
                        }
                        if let Some(action) = view.menu_key(key, &agent.cancel) {
                            match action {
                                MenuAction::Quit if update.is_none() => break,
                                MenuAction::Quit => view.status = "Wait for the Forge update to finish before quitting".into(),
                                MenuAction::Update if update.is_none() => {
                                    view.status = "Fetching Forge source from GitHub…".into();
                                    update = Some(tokio::spawn(crate::update::update_source()));
                                }
                                MenuAction::Update | MenuAction::Handled => {}
                            }
                            continue;
                        }
                        match key.code{
                            KeyCode::Esc if view.busy=>agent.cancel.cancel(),
                            KeyCode::Tab=>view.panels = !view.panels,
                            KeyCode::PageUp=>view.scroll=view.scroll.saturating_add(10),
                            KeyCode::PageDown=>view.scroll=view.scroll.saturating_sub(10),
                            KeyCode::Enter if key.modifiers.intersects(KeyModifiers::ALT|KeyModifiers::SHIFT)=>view.insert("\n"),
                            KeyCode::Enter if update.is_none()&&!view.busy&&view.slash_query().is_none()&&!view.input.trim().is_empty()=>{
                                let prompt=std::mem::take(&mut view.input);view.cursor=0;view.transcript.push_str(&format!("\nYOU\n{prompt}\n"));view.scroll=0;view.busy=true;
                                agent.cancel=CancellationToken::new();let worker=agent.clone();
                                task=Some(tokio::spawn(async move{let _=worker.turn(&prompt).await;}));
                            }
                            KeyCode::Backspace=>view.backspace(),
                            KeyCode::Delete=>{if view.cursor<view.input.len(){view.input.remove(view.cursor);view.selection=0;view.dismiss_slash=false;}}
                            KeyCode::Left=>view.left(),
                            KeyCode::Right=>view.right(),
                            KeyCode::Home=>view.cursor=view.input[..view.cursor].rfind('\n').map(|i|i+1).unwrap_or(0),
                            KeyCode::End=>view.cursor+=view.input[view.cursor..].find('\n').unwrap_or(view.input.len()-view.cursor),
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL)=>{view.input.clear();view.cursor=0;}
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL)=>view.insert(&c.to_string()),
                            _=>{},
                        }
                    }
                    _=>{},
                }
            }
        }
    }
    agent.cancel.cancel();
    if let Some(job) = update {
        job.abort();
        let _ = job.await;
    }
    if let Some(p) = view.pending.take() {
        let _ = p.reply.send(Some(false));
    }
    if let Some(task) = task {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
    }
    Ok(())
}

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title)
}
fn draw(frame: &mut Frame, view: &View) {
    let area = frame.area();
    let composer_height = view.input.lines().count().saturating_add(2).clamp(3, 7) as u16;
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND).fg(FOREGROUND)),
        area,
    );
    if view.transcript.is_empty() {
        draw_home(frame, view, composer_height);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(2),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" FORGE ", Style::default().fg(Color::Cyan)),
            Span::raw(format!(" / {} · {}", view.model, view.workspace)),
        ])),
        rows[0],
    );
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if view.panels && area.width >= 100 {
            vec![Constraint::Percentage(73), Constraint::Percentage(27)]
        } else {
            vec![Constraint::Percentage(100)]
        })
        .split(rows[1]);
    let conversation = Paragraph::new(view.transcript.as_str())
        .block(panel(" Conversation "))
        .wrap(Wrap { trim: false });
    let height = conversation
        .line_count(cols[0].width)
        .min(u16::MAX as usize) as u16;
    let offset = height
        .saturating_sub(cols[0].height.saturating_sub(2))
        .saturating_sub(view.scroll);
    frame.render_widget(conversation.scroll((offset, 0)), cols[0]);
    if cols.len() > 1 {
        let side = Layout::default()
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(cols[1]);
        let activity = view
            .activity
            .iter()
            .rev()
            .take(12)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        frame.render_widget(
            Paragraph::new(activity)
                .block(panel(" Activity "))
                .wrap(Wrap { trim: false }),
            side[0],
        );
        frame.render_widget(
            Paragraph::new(view.usage.as_str())
                .block(panel(" Request usage · USD estimate "))
                .wrap(Wrap { trim: false }),
            side[1],
        );
    }
    draw_composer(frame, view, rows[2]);
    frame.render_widget(
        Paragraph::new(format!(
            " {} · {} · Ctrl+P commands · PgUp/PgDn scroll",
            view.status,
            &view.session[..view.session.len().min(8)]
        ))
        .style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
    if let Some(p) = &view.pending {
        let rect = center(area, 90, 85);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(format!(
                "{}\n\ny approve · n deny · ↑/↓ scroll\n\n{}",
                p.title, p.preview
            ))
            .block(panel(" Review action ").border_style(Style::default().fg(Color::Cyan)))
            .wrap(Wrap { trim: false })
            .scroll((p.scroll, 0)),
            rect,
        );
    } else if view.menu_open() {
        draw_commands(frame, view, rows[2]);
    }
}

const BACKGROUND: Color = Color::Rgb(10, 10, 10);
const SURFACE: Color = Color::Rgb(29, 29, 29);
const FOREGROUND: Color = Color::Rgb(232, 232, 232);
const MUTED: Color = Color::Rgb(143, 143, 143);
const ACCENT: Color = Color::Rgb(103, 159, 241);

// The terminal supplies the glyphs. The lockup uses square block cells so it
// travels with Forge across terminal fonts and needs no graphics protocol.
const ANVIL: [&str; 4] = [
    "▀▀█████████▌  ",
    "  ▀███████▀   ",
    "     ███      ",
    "   ▄█████▄    ",
];
const WORDMARK: [&str; 4] = [
    "█▀▀▀  █▀▀█  █▀▀█  █▀▀▀  █▀▀▀",
    "█▄▄   █  █  █▄▄█  █ ▄▄  █▄▄ ",
    "█     █  █  █ ▀▄  █  █  █   ",
    "▀     ▀▀▀▀  ▀  ▀  ▀▀▀▀  ▀▀▀▀",
];

fn draw_home(frame: &mut Frame, view: &View, composer_height: u16) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(94);
    let large_logo = width >= 44 && area.height >= 14;
    let logo_height = if large_logo { 4 } else { 1 };
    let height = logo_height + composer_height + 4;
    let x = area.x + (area.width - width) / 2;
    let y = area.y + area.height.saturating_sub(height) * 2 / 5;
    if large_logo {
        let lines = ANVIL
            .iter()
            .zip(WORDMARK)
            .map(|(anvil, word)| {
                Line::from(vec![
                    Span::styled(*anvil, Style::default().fg(MUTED)),
                    Span::styled(word, Style::default().fg(FOREGROUND)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).centered(),
            Rect::new(x, y, width, logo_height),
        );
    } else {
        frame.render_widget(
            Paragraph::new("FORGE")
                .centered()
                .style(Style::default().fg(FOREGROUND)),
            Rect::new(x, y, width, 1),
        );
    }
    let composer_y = y + logo_height + 2;
    let available = area.bottom().saturating_sub(composer_y);
    let composer = Rect::new(
        x,
        composer_y.min(area.bottom()),
        width,
        composer_height.min(available),
    );
    draw_composer(frame, view, composer);
    if composer.bottom() < area.bottom() {
        let hint = if width >= 52 {
            "/ commands   Ctrl+P palette   Enter send"
        } else {
            "/ commands   Enter send"
        };
        frame.render_widget(
            Paragraph::new(hint)
                .right_aligned()
                .style(Style::default().fg(MUTED)),
            Rect::new(x, composer.bottom(), width, 1),
        );
    }
    if composer.bottom().saturating_add(1) < area.bottom() && view.status != "Ready" {
        frame.render_widget(
            Paragraph::new(view.status.as_str()).style(Style::default().fg(MUTED)),
            Rect::new(x, composer.bottom() + 1, width, 1),
        );
    }
    if view.menu_open() {
        draw_commands(frame, view, composer);
    }
}

fn draw_composer(frame: &mut Frame, view: &View, area: Rect) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    frame.render_widget(Block::default().style(Style::default().bg(SURFACE)), area);
    frame.render_widget(
        Paragraph::new(vec![Line::from("▏"); area.height as usize])
            .style(Style::default().fg(ACCENT)),
        Rect::new(area.x, area.y, 1, area.height),
    );
    let editor = Rect::new(area.x + 2, area.y, area.width - 3, area.height - 1);
    let before = &view.input[..view.cursor];
    let cursor_y = before.bytes().filter(|&byte| byte == b'\n').count();
    let cursor_y = cursor_y.min(u16::MAX as usize) as u16;
    let cursor_x = before
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .width()
        .min(u16::MAX as usize) as u16;
    let scroll_y = cursor_y.saturating_sub(editor.height - 1);
    let scroll_x = cursor_x.saturating_sub(editor.width - 1);
    let text = if view.input.is_empty() {
        "Message Forge… (/ for commands)"
    } else {
        &view.input
    };
    let color = if view.input.is_empty() {
        MUTED
    } else {
        FOREGROUND
    };
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(color))
            .scroll((scroll_y, scroll_x)),
        editor,
    );
    let metadata = Line::from(vec![
        Span::styled(
            if view.busy { "Working" } else { "Build" },
            Style::default().fg(ACCENT),
        ),
        Span::raw(format!("  {}", view.model)),
    ]);
    frame.render_widget(
        Paragraph::new(metadata).style(Style::default().fg(FOREGROUND)),
        Rect::new(editor.x, area.bottom() - 1, editor.width, 1),
    );
    if view.pending.is_none() && !view.palette {
        frame.set_cursor_position((
            editor.x + cursor_x - scroll_x,
            editor.y + cursor_y - scroll_y,
        ));
    }
}

fn draw_commands(frame: &mut Frame, view: &View, composer: Rect) {
    let commands = view.commands();
    let bounds = frame.area();
    let height = (commands.len().max(1) as u16 + 2).min(bounds.height);
    let y = if composer.bottom().saturating_add(height) <= bounds.bottom() {
        composer.bottom()
    } else {
        composer.y.saturating_sub(height).max(bounds.y)
    };
    let area = Rect::new(composer.x, y, composer.width.min(54), height);
    let lines = if commands.is_empty() {
        vec![Line::styled(
            " No matching commands",
            Style::default().fg(MUTED),
        )]
    } else {
        commands
            .iter()
            .enumerate()
            .map(|(index, (_, name, description))| {
                let selected = index == view.selection;
                let style = if selected {
                    Style::default().fg(FOREGROUND).bg(Color::Rgb(41, 54, 74))
                } else {
                    Style::default().fg(MUTED)
                };
                Line::styled(format!(" {name:<9} {description}"), style)
            })
            .collect()
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(" Commands ").style(Style::default().bg(SURFACE).fg(FOREGROUND))),
        area,
    );
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let w = area.width.saturating_mul(width) / 100;
    let h = area.height.saturating_mul(height) / 100;
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> View {
        View {
            transcript: "FORGE\nhello".into(),
            input: "multi\nline".into(),
            cursor: 10,
            activity: vec!["read_file".into()],
            usage: "10 input\n20 output".into(),
            busy: false,
            panels: true,
            palette: false,
            selection: 0,
            dismiss_slash: false,
            pending: None,
            scroll: 0,
            status: "Ready".into(),
            session: "12345678".into(),
            model: "test".into(),
            workspace: "repo".into(),
        }
    }
    #[test]
    fn normal_and_narrow_layouts() {
        let view = fixture();
        for (w, h) in [(120, 35), (45, 15), (20, 8)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &view)).unwrap();
            let rendered = t
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(rendered.contains("FORGE"));
            assert!(rendered.contains("Build"));
        }
    }
    #[test]
    fn unicode_composer_edits_at_character_boundaries() {
        let mut v = fixture();
        v.input.clear();
        v.cursor = 0;
        v.insert("é中\nhello");
        v.cursor = "é中".len();
        v.backspace();
        assert_eq!(v.input, "é\nhello");
        v.left();
        assert_eq!(v.cursor, 0);
        v.right();
        assert_eq!(v.cursor, "é".len());
        v.insert("\nline");
        assert_eq!(v.input, "é\nline\nhello");
    }
    #[test]
    fn approval_and_command_palette_render() {
        let mut v = fixture();
        let (tx, _) = oneshot::channel();
        v.pending = Some(Pending {
            title: "Apply patch".into(),
            preview: "-before\n+after".into(),
            reply: tx,
            scroll: 0,
        });
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &v)).unwrap();
        let content = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains("Review action"));
        assert!(content.contains("+after"));
        v.pending = None;
        v.palette = true;
        t.draw(|f| draw(f, &v)).unwrap();
        let content = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains("Commands"));
        assert!(content.contains("Toggle activity"));
    }
}
