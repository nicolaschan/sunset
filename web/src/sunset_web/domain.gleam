//// Domain types backing the chat UI.
////
//// Field names already use vocabulary that overlaps with the eventual
//// sunset-store / sunset-core mapping (e.g. `RoomId(String)`, message
//// `id` is a content-addressed string, member `you` is a flag rather
//// than computed from a separate session). Once the chat-domain spec
//// lands these become aliases over the real `(verifying_key, name)`
//// pairs, and the field shapes shouldn't shift.

import gleam/dict.{type Dict}
import gleam/option

pub type RoomId {
  RoomId(String)
}

pub type ChannelId {
  ChannelId(String)
}

pub type MemberId {
  MemberId(String)
}

pub type ConnStatus {
  Connected
  Reconnecting
  Offline
}

pub type ChannelKind {
  TextChannel
  Voice
}

pub type Presence {
  Online
  Speaking
  MutedP
  Away
  OfflineP
}

/// Routing state for a peer. v1 just renders these; the resolution to
/// real network state lives in sunset-sync (later plan).
pub type RelayStatus {
  Direct
  OneHop
  TwoHop
  ViaPeer(String)
  SelfRelay
  NoRelay
}

pub type Room {
  Room(
    id: RoomId,
    name: String,
    members: Int,
    online: Int,
    in_call: Int,
    status: ConnStatus,
    last_active: String,
    unread: Int,
  )
}

pub type Channel {
  Channel(
    id: ChannelId,
    name: String,
    kind: ChannelKind,
    in_call: Int,
    unread: Int,
  )
}

pub type Member {
  Member(
    id: MemberId,
    name: String,
    initials: String,
    status: Presence,
    relay: RelayStatus,
    you: Bool,
    in_call: Bool,
    role: RoleOpt,
    /// Unix-ms timestamp of the last app-level presence heartbeat we
    /// received from this peer. `None` for self or peers we have not
    /// heard from. The popover renders age as `now_ms - this`.
    last_heartbeat_ms: option.Option(Int),
    /// Raw display name from the wasm side. `None` ⇒ peer hasn't set
    /// one (the rendered `name` field above falls back to short_pubkey
    /// in that case).
    raw_name: option.Option(String),
    /// Raw pubkey bytes — kept here so MembersUpdated can map raw
    /// names by pubkey without re-deriving from MemberId.
    pubkey: BitArray,
  )
}

pub type Reaction {
  Reaction(emoji: String, count: Int, by_you: Bool)
}

/// Per-recipient delivery confirmation, surfaced in the message-details
/// side panel.
pub type Receipt {
  Receipt(name: String, time: String, relay: RelayStatus)
}

/// Cryptographic + delivery metadata available for messages we have
/// full provenance on. In v1 only own outgoing messages have this; the
/// chat-domain plan will populate it from real signed entries later.
pub type MessageDetails {
  MessageDetails(
    sender: String,
    message_id: String,
    prev_id: String,
    signature: String,
    verified: Bool,
    hops: List(String),
    sent_at: String,
    delivered_at: String,
    receipts: List(Receipt),
  )
}

pub type DetailsOpt {
  HasDetails(MessageDetails)
  NoDetails
}

/// Per-recipient voice tweaks the local user has applied to a peer
/// in an active call. Mutated via the voice-member popover.
pub type VoiceSettings {
  VoiceSettings(
    /// Playback volume for this peer's incoming stream as a percent.
    /// 100 = unity. For other peers we allow 0-200%; for the user's
    /// own outgoing channel the popover narrows the slider to 0-100%.
    volume: Int,
    /// Whether incoming-stream denoising is enabled for this peer.
    denoise: Bool,
    /// Whether this peer is muted locally ("mute for me"). Doesn't
    /// affect anyone else.
    deafened: Bool,
  )
}

pub type Message {
  Message(
    id: String,
    author_pubkey: BitArray,
    initials: String,
    time: String,
    body: String,
    seen_by: Int,
    you: Bool,
    pending: Bool,
    reactions: List(Reaction),
    details: DetailsOpt,
  )
}

/// Pre-resolved view of a Message with the author display name baked in.
/// Built once per render from the live name_map; view functions consume
/// these instead of raw Message so they don't have to thread the dict around.
pub type MessageView {
  MessageView(
    id: String,
    author: String,
    initials: String,
    time: String,
    body: String,
    seen_by: Int,
    you: Bool,
    pending: Bool,
    reactions: List(Reaction),
    details: DetailsOpt,
  )
}

pub type RoleOpt {
  HasRole(String)
  NoRole
}

/// Viewport class derived from `matchMedia("(max-width: 767px)")`.
/// Updated on init and on resize. Phone gates the entire mobile
/// layout branch in `shell.view`.
pub type Viewport {
  Phone
  Desktop
}

/// Drawer that's currently open on phone. Carried as `Option(Drawer)`
/// on the model; `None` means closed. Desktop ignores this field
/// because drawers don't render on desktop. Channels↔rooms is modeled
/// as a swap (replacing the field's value), not a stack.
pub type Drawer {
  RoomsDrawer
  ChannelsDrawer
  MembersDrawer
}

/// Bottom sheet currently open on phone. Carried as `Option(Sheet)` on
/// the model; `None` means closed. Replaces two separate optional
/// fields (detail message id, voice popover member name) so the model
/// can't end up with both the details panel AND the voice popover up
/// at the same time.
pub type Sheet {
  DetailsSheet(message_id: String)
  VoiceSheet(member_name: String)
}

/// Live peer state emitted by the voice runtime's PeerStateSink, keyed
/// by peer pubkey hex. Updated from `on_voice_peer_state` callbacks.
pub type VoicePeerStateUI {
  VoicePeerStateUI(in_call: Bool, talking: Bool, is_muted: Bool)
}

/// Top-level voice subsystem state on the Lustre model.
pub type VoiceModel {
  VoiceModel(
    /// `None` = not in call; `Some(room_id)` = active voice session for that room.
    self_in_call: option.Option(RoomId),
    self_muted: Bool,
    self_deafened: Bool,
    /// Receiver-side RNNoise denoising. Default `True`; the runtime
    /// starts with denoising on, so the UI toggle reflects that.
    denoise: Bool,
    /// Per-peer state keyed by pubkey hex string.
    peers: Dict(String, VoicePeerStateUI),
    /// Set when mic permission is denied; cleared by `ResetVoiceError`.
    permission_error: option.Option(String),
  )
}

pub type RelayConnState {
  RelayConnecting
  RelayConnected
  RelayBackoff
  RelayCancelled
}

/// View-model for a relay row + popover. Derived per render from
/// `Model.intents` via `relays_view.relays_for_view`. Not a source
/// of truth — `intents` remains so.
pub type Relay {
  Relay(
    /// IntentId — popover key.
    id: Float,
    /// Parsed hostname for display, e.g. "relay.sunset.chat".
    host: String,
    /// Full Connectable label (raw user input or canonical URL).
    raw_label: String,
    state: RelayConnState,
    attempt: Int,
    /// First 4 + last 4 hex bytes of the relay's peer_id. None
    /// while the Noise handshake is still pending.
    peer_id_short: option.Option(String),
    /// Wall-clock ms of the most recent Pong from this relay.
    last_pong_at_ms: option.Option(Int),
    /// Round-trip time of the most recent Pong, in milliseconds.
    last_rtt_ms: option.Option(Int),
  )
}
