//! /command parser. Pure — no async, no I/O.

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Join(String),
    Switch(String),
    Leave(Option<String>),
    Rooms,
    Peers,
    Relays,
    RelayAdd(String),
    Name(String),
    Me,
    Voice,
    Quit,
    Send(String),
    Unknown(String),
    Empty,
}

pub fn parse(line: &str) -> Command {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return Command::Empty;
    }
    if !trimmed.starts_with('/') {
        return Command::Send(line.trim_end().to_owned());
    }
    let mut parts = trimmed[1..].splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("").trim();
    match head {
        "help" | "?" => Command::Help,
        "join" => {
            if tail.is_empty() {
                Command::Unknown("/join".to_owned())
            } else {
                Command::Join(tail.to_owned())
            }
        }
        "switch" => {
            if tail.is_empty() {
                Command::Unknown("/switch".to_owned())
            } else {
                Command::Switch(tail.to_owned())
            }
        }
        "leave" => {
            if tail.is_empty() {
                Command::Leave(None)
            } else {
                Command::Leave(Some(tail.to_owned()))
            }
        }
        "rooms" => Command::Rooms,
        "peers" => Command::Peers,
        "relays" => Command::Relays,
        "relay" => {
            let mut sub = tail.splitn(2, char::is_whitespace);
            let verb = sub.next().unwrap_or("");
            let arg = sub.next().unwrap_or("").trim();
            match verb {
                "add" if !arg.is_empty() => Command::RelayAdd(arg.to_owned()),
                _ => Command::Unknown(format!("/relay {tail}").trim_end().to_owned()),
            }
        }
        "name" => Command::Name(tail.to_owned()),
        "me" => Command::Me,
        "voice" => Command::Voice,
        "quit" | "exit" => Command::Quit,
        other => {
            let suffix = if tail.is_empty() {
                String::new()
            } else {
                format!(" {tail}")
            };
            Command::Unknown(format!("/{other}{suffix}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_empty() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("   "), Command::Empty);
    }

    #[test]
    fn bare_text_is_send() {
        assert_eq!(parse("hello world"), Command::Send("hello world".to_owned()));
    }

    #[test]
    fn slash_help_is_help() {
        assert_eq!(parse("/help"), Command::Help);
        assert_eq!(parse("/?"), Command::Help);
    }

    #[test]
    fn slash_join_takes_room_name() {
        assert_eq!(parse("/join alpha"), Command::Join("alpha".to_owned()));
    }

    #[test]
    fn slash_join_without_arg_is_unknown() {
        assert!(matches!(parse("/join"), Command::Unknown(_)));
    }

    #[test]
    fn slash_relay_add_takes_url() {
        assert_eq!(
            parse("/relay add wss://r.example#x25519=ab"),
            Command::RelayAdd("wss://r.example#x25519=ab".to_owned())
        );
    }

    #[test]
    fn slash_relay_without_subcommand_is_unknown() {
        assert!(matches!(parse("/relay"), Command::Unknown(_)));
        assert!(matches!(parse("/relay add"), Command::Unknown(_)));
    }

    #[test]
    fn slash_leave_optional_arg() {
        assert_eq!(parse("/leave"), Command::Leave(None));
        assert_eq!(
            parse("/leave alpha"),
            Command::Leave(Some("alpha".to_owned()))
        );
    }

    #[test]
    fn unknown_slash_command() {
        assert!(matches!(parse("/foo bar"), Command::Unknown(_)));
    }

    #[test]
    fn quit_aliases() {
        assert_eq!(parse("/quit"), Command::Quit);
        assert_eq!(parse("/exit"), Command::Quit);
    }
}
