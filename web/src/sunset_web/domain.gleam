//// Domain types backing the chat UI.
////
//// Field names already use vocabulary that overlaps with the eventual
//// sunset-store / sunset-core mapping (e.g. `RoomId(String)`, message
//// `id` is a content-addressed string, member `you` is a flag rather
//// than computed from a separate session). Once the chat-domain spec
//// lands these become aliases over the real `(verifying_key, name)`
//// pairs, and the field shapes shouldn't shift.

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
  Bridge(BridgeKind)
}

pub type BridgeKind {
  Minecraft
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
  BridgeRelay
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
    bridge: BridgeOpt,
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
    bridge: BridgeOpt,
    role: RoleOpt,
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

pub type Message {
  Message(
    id: String,
    author: String,
    initials: String,
    time: String,
    body: String,
    seen_by: Int,
    you: Bool,
    pending: Bool,
    reactions: List(Reaction),
    bridge: BridgeOpt,
    details: DetailsOpt,
  )
}

/// Tiny Option-substitutes — Gleam's `option.Option` works, but in
/// patterns and view code these read more naturally as plain ADTs.
pub type BridgeOpt {
  HasBridge(BridgeKind)
  NoBridge
}

pub type RoleOpt {
  HasRole(String)
  NoRole
}
