//! Snapshot types passed from `client::Client` to `ui::App`.
//!
//! Plain data — no callbacks, no async. The Client mutates these
//! through `Rc<RefCell<...>>` cells; the UI reads them on each
//! redraw. UI re-render is signaled out-of-band via a
//! `tokio::sync::Notify`, not via these types.

#[derive(Debug, Clone)]
pub struct MessageLine {
    pub author_pubkey: [u8; 32],
    pub author_name: Option<String>,
    pub body: String,
    pub sent_at_ms: u64,
    pub is_self: bool,
}

#[derive(Debug, Clone)]
pub struct MemberRow {
    pub pubkey: [u8; 32],
    pub name: Option<String>,
    /// "self" | "direct" | "via_relay" | "unknown" — matches
    /// `sunset_core::membership::ConnectionMode::as_str`.
    pub connection_mode: &'static str,
    /// "online" | "away" | "offline" — matches
    /// `sunset_core::membership::Presence::as_str`.
    pub presence: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct RoomView {
    pub name: String,
    pub messages: Vec<MessageLine>,
    pub members: Vec<MemberRow>,
}

#[derive(Debug, Clone)]
pub struct RelayRow {
    pub label: String,
    /// "connecting" | "connected" | "backoff" | "error" | "stopped"
    pub state: &'static str,
    pub last_rtt_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct TopState {
    pub identity_hex: String,
    pub self_name: Option<String>,
    pub active_room: Option<String>,
    pub open_rooms: Vec<String>,
    pub relays: Vec<RelayRow>,
    /// Append-only log printed in the message pane in addition to
    /// chat messages. Used for `/help`, errors, and command output.
    pub system_log: Vec<String>,
}
