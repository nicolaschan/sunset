//! Dispatches a parsed `Command` to side effects on a `Client`.

use std::rc::Rc;

use crate::client::Client;
use crate::command::Command;

pub enum DispatchOutcome {
    Continue,
    Quit,
}

pub async fn dispatch(client: &Rc<Client>, cmd: Command) -> DispatchOutcome {
    match cmd {
        Command::Empty => {}
        Command::Help => {
            for line in HELP_LINES {
                client.append_system((*line).to_owned());
            }
        }
        Command::Join(name) => {
            if let Err(e) = client.join_room(&name).await {
                client.append_system(format!("/join failed: {e}"));
            }
        }
        Command::Switch(name) => {
            if client.snapshot_room(&name).is_some() {
                client.set_active(&name);
            } else {
                client.append_system(format!("/switch: not in room '{name}' — use /join"));
            }
        }
        Command::Leave(name) => {
            let target = name.or_else(|| client.snapshot_top().active_room);
            if let Some(t) = target {
                client.leave_room(&t);
            }
        }
        Command::Rooms => {
            let top = client.snapshot_top();
            let list = if top.open_rooms.is_empty() {
                "(none)".to_owned()
            } else {
                top.open_rooms
                    .iter()
                    .map(|r| format!("#{r}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            client.append_system(format!("rooms: {list}"));
        }
        Command::Peers => {
            let top = client.snapshot_top();
            if let Some(active) = &top.active_room {
                if let Some(view) = client.snapshot_room(active) {
                    if view.members.is_empty() {
                        client.append_system("(no members yet)".to_owned());
                    } else {
                        for m in &view.members {
                            let name = m.name.as_deref().unwrap_or("(no name)");
                            client.append_system(format!(
                                "peer {} {} ({}) {}",
                                &hex::encode(m.pubkey)[..8],
                                name,
                                m.connection_mode,
                                m.presence,
                            ));
                        }
                    }
                }
            } else {
                client.append_system("/peers: no active room".to_owned());
            }
        }
        Command::Relays => {
            let top = client.snapshot_top();
            if top.relays.is_empty() {
                client.append_system("(no relay intents)".to_owned());
            } else {
                for r in &top.relays {
                    let rtt = match r.last_rtt_ms {
                        Some(v) => format!("{v}ms"),
                        None => "—".to_owned(),
                    };
                    client.append_system(format!("relay {} [{}] rtt={}", r.label, r.state, rtt));
                }
            }
        }
        Command::RelayAdd(url) => {
            if let Err(e) = client.add_relay(url).await {
                client.append_system(format!("/relay add failed: {e}"));
            }
        }
        Command::Name(name) => client.set_self_name(&name),
        Command::Me => {
            let top = client.snapshot_top();
            let label = top.self_name.as_deref().unwrap_or("(no name)");
            client.append_system(format!("identity {} ({})", top.identity_hex, label));
        }
        Command::Voice => {
            client.append_system(
                "/voice: not yet implemented in the CLI; use the web client at https://sunset.chat"
                    .to_owned(),
            );
        }
        Command::Quit => return DispatchOutcome::Quit,
        Command::Send(body) => {
            if let Err(e) = client.send_text(body).await {
                client.append_system(format!("send failed: {e}"));
            }
        }
        Command::Unknown(s) => {
            client.append_system(format!("unknown command: {s} — try /help"));
        }
    }
    DispatchOutcome::Continue
}

const HELP_LINES: &[&str] = &[
    "commands:",
    "  /help                — this list",
    "  /join <room>         — open and switch to a room",
    "  /switch <room>       — switch active room",
    "  /leave [room]        — leave a room (default: active)",
    "  /rooms               — list open rooms",
    "  /peers               — list peers in the active room",
    "  /relays              — list relay intents + state",
    "  /relay add <url>     — add a relay (ws://, wss://, wt://, wts://, or hostname)",
    "  /name <name>         — set your display name",
    "  /me                  — show your identity",
    "  /voice               — voice (deferred — use the web client)",
    "  /quit                — exit",
];
