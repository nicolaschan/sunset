//! Pure rendering: TopState + RoomView + composer → ratatui Frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::view::{RoomView, TopState};

pub struct ComposerState<'a> {
    pub buffer: &'a str,
    /// Cursor column relative to the buffer start.
    pub cursor: u16,
}

pub fn draw(frame: &mut Frame, top: &TopState, room: Option<&RoomView>, composer: &ComposerState) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
    draw_title(frame, chunks[0], top);
    draw_main(frame, chunks[1], top, room);
    draw_composer(frame, chunks[2], composer);
}

fn draw_title(frame: &mut Frame, area: Rect, top: &TopState) {
    let title = match &top.active_room {
        Some(r) => format!(" sunset.chat — #{r}"),
        None => " sunset.chat".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_main(frame: &mut Frame, area: Rect, top: &TopState, room: Option<&RoomView>) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(1)])
        .split(area);
    draw_left_rail(frame, cols[0], top, room);
    draw_messages(frame, cols[1], top, room);
}

fn draw_left_rail(frame: &mut Frame, area: Rect, top: &TopState, room: Option<&RoomView>) {
    let rooms_h = (top.open_rooms.len() as u16).saturating_add(2).max(3);
    let relays_h = (top.relays.len() as u16).saturating_add(2).max(3);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(rooms_h),
            Constraint::Min(1),
            Constraint::Length(relays_h),
        ])
        .split(area);
    let active = top.active_room.as_deref();
    let room_items: Vec<ListItem> = top
        .open_rooms
        .iter()
        .map(|r| {
            let marker = if Some(r.as_str()) == active { ">" } else { " " };
            ListItem::new(format!("{marker} #{r}"))
        })
        .collect();
    frame.render_widget(
        List::new(room_items).block(Block::default().borders(Borders::ALL).title("rooms")),
        rows[0],
    );

    let peer_items: Vec<ListItem> = match room {
        Some(v) => v
            .members
            .iter()
            .map(|m| {
                let glyph = match m.connection_mode {
                    "self" => 'M',
                    "direct" => 'D',
                    "via_relay" => 'R',
                    _ => '?',
                };
                let name = m.name.as_deref().unwrap_or("(no name)");
                ListItem::new(format!("{glyph} {name}"))
            })
            .collect(),
        None => Vec::new(),
    };
    frame.render_widget(
        List::new(peer_items).block(Block::default().borders(Borders::ALL).title("peers")),
        rows[1],
    );

    let relay_items: Vec<ListItem> = top
        .relays
        .iter()
        .map(|r| {
            let glyph = match r.state {
                "connected" => '+',
                "connecting" => '~',
                "backoff" => '.',
                _ => '!',
            };
            ListItem::new(format!("{glyph} {}", short_label(&r.label)))
        })
        .collect();
    frame.render_widget(
        List::new(relay_items).block(Block::default().borders(Borders::ALL).title("relays")),
        rows[2],
    );
}

fn draw_messages(frame: &mut Frame, area: Rect, top: &TopState, room: Option<&RoomView>) {
    let mut lines: Vec<String> = Vec::new();
    for sys in &top.system_log {
        lines.push(format!("· {sys}"));
    }
    if let Some(v) = room {
        for m in &v.messages {
            let who = m
                .author_name
                .clone()
                .unwrap_or_else(|| short_pubkey(&m.author_pubkey));
            let when = format_time_ms(m.sent_at_ms);
            lines.push(format!("{when}  {who}: {}", m.body));
        }
    }
    let h = area.height.saturating_sub(2) as usize;
    let start = lines.len().saturating_sub(h);
    let visible = lines[start..].join("\n");
    frame.render_widget(
        Paragraph::new(visible).block(
            Block::default()
                .borders(Borders::ALL)
                .title(top.active_room.clone().unwrap_or_else(|| " ".to_owned())),
        ),
        area,
    );
}

fn draw_composer(frame: &mut Frame, area: Rect, composer: &ComposerState) {
    let text = format!("> {}", composer.buffer);
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
    // Cursor: 1 (left border) + 2 ("> ") + composer.cursor.
    let x = area.x + 1 + 2 + composer.cursor;
    let y = area.y + 1;
    frame.set_cursor_position((x, y));
}

fn short_pubkey(pk: &[u8; 32]) -> String {
    hex::encode(&pk[..4])
}

fn short_label(s: &str) -> String {
    if s.len() <= 18 {
        s.to_owned()
    } else {
        format!("{}…", &s[..17])
    }
}

fn format_time_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    let dt = chrono::DateTime::from_timestamp(secs, nsecs).unwrap_or_default();
    let local: chrono::DateTime<chrono::Local> = dt.into();
    local.format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::view::{MemberRow, MessageLine, RelayRow, RoomView, TopState};

    fn buffer_lines(buf: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(buf.area.x + x, buf.area.y + y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn renders_active_room_in_title() {
        let backend = TestBackend::new(60, 14);
        let mut term = Terminal::new(backend).unwrap();

        let top = TopState {
            identity_hex: "deadbeef".into(),
            self_name: Some("alice".into()),
            active_room: Some("alpha".into()),
            open_rooms: vec!["alpha".into(), "beta".into()],
            relays: vec![RelayRow {
                label: "relay.example.com".into(),
                state: "connected",
                last_rtt_ms: Some(12),
            }],
            system_log: vec!["welcome".into()],
        };
        let view = RoomView {
            name: "alpha".into(),
            messages: vec![MessageLine {
                author_pubkey: [0x11; 32],
                author_name: Some("bob".into()),
                body: "hello".into(),
                sent_at_ms: 1_700_000_000_000,
                is_self: false,
            }],
            members: vec![MemberRow {
                pubkey: [0x11; 32],
                name: Some("bob".into()),
                connection_mode: "via_relay",
                presence: "online",
            }],
        };
        let composer = ComposerState {
            buffer: "/help",
            cursor: 5,
        };
        term.draw(|f| draw(f, &top, Some(&view), &composer))
            .unwrap();
        let lines = buffer_lines(term.backend().buffer());
        assert!(lines[0].contains("sunset.chat"), "title: {}", lines[0]);
        assert!(lines[0].contains("alpha"), "title room: {}", lines[0]);
        assert!(
            lines.iter().any(|l| l.contains("> #alpha")),
            "active room marker missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("R bob")),
            "via_relay glyph + name missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("bob: hello")),
            "message missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("> /help")),
            "composer missing"
        );
    }
}
