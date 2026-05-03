//// sunset.chat — Gleam + Lustre frontend.
////
//// Two top-level views:
////   * `LandingView` — empty state shown at root `/`. The user types a
////     room name and submits; we add it to their joined-rooms list and
////     navigate.
////   * `RoomView(name)` — the existing 4-column chat shell rendering
////     fixture data for the named room.
////
//// Routing is anchor-based: the URL fragment (`/#dusk-collective`) is
//// the source of truth for which room is active. A storage FFI shim
//// persists the joined-rooms list + last-used room to localStorage
//// so a refresh restores the user's state.

import gleam/dict.{type Dict}
import gleam/io
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/set.{type Set}
import gleam/string
import lustre
import lustre/attribute
import lustre/effect.{type Effect}
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type ChannelId, type Message, type Room, ChannelId, NoBridge, Room, RoomId,
}
import sunset_web/fixture
import sunset_web/scroll_anchor
import sunset_web/storage
import sunset_web/sunset.{type ClientHandle, type IncomingMessage}
import sunset_web/composer
import sunset_web/markdown
import sunset_web/theme.{type Mode, Dark, Light}
import sunset_web/ui
import sunset_web/views/bottom_sheet
import sunset_web/views/channels
import sunset_web/views/details_panel
import sunset_web/views/emoji_picker
import sunset_web/views/landing
import sunset_web/views/main_panel
import sunset_web/views/members
import sunset_web/views/peer_status_popover
import sunset_web/views/phone_header
import sunset_web/views/rooms
import sunset_web/views/shell
import sunset_web/views/touch_drag
import sunset_web/views/voice_minibar
import sunset_web/views/voice_popover

/// Relays the client dials at startup when the URL has no
/// `?relay=…` query parameter. Each entry is fed through
/// `sunset-relay-resolver`, so bare hostnames work.
const default_relays: List(String) = ["relay.sunset.chat"]

pub type View {
  LandingView
  RoomView(name: String)
}

pub type Model {
  Model(
    mode: Mode,
    view: View,
    joined_rooms: List(String),
    rooms_collapsed: Bool,
    landing_input: String,
    sidebar_search: String,
    current_channel: ChannelId,
    draft: String,
    reacting_to: Option(String),
    /// Target id whose full emoji picker is currently open, if any.
    full_picker_for: Option(String),
    /// Per-target reaction state from the bridge tracker. Whole-snapshot
    /// replacement on each `ReactionsChanged` — never partially merged in
    /// the FE; the core tracker is the source of truth. Shape:
    /// `Dict(target_hex, Dict(emoji, Set(author_pubkey_hex)))`.
    reactions: Dict(String, Dict(String, Set(String))),
    /// Name of the room currently being dragged in the rooms rail.
    /// `None` between drag operations.
    dragging_room: Option(String),
    /// Name of the room currently hovered over while dragging — used
    /// for the visible drop-target indicator.
    drag_over_room: Option(String),
    /// Per-member voice tweaks (volume / denoise / deafened),
    /// keyed by member name. Seeded from the fixture once.
    voice_settings: Dict(String, domain.VoiceSettings),
    /// Engine handle. None until the wasm bundle finishes initialising.
    client: Option(ClientHandle),
    /// Real chat messages received from the engine. Empty on first load.
    messages: List(domain.Message),
    /// Relay connection status: "disconnected", "connecting", "connected", "error".
    relay_status: String,
    /// Live members list from the presence tracker. Empty on first load.
    members: List(domain.Member),
    /// Viewport class — drives the desktop/phone branch in `shell.view`.
    viewport: domain.Viewport,
    /// Currently open drawer on phone. Ignored on desktop. Channels and
    /// rooms drawers cross-transition (one swaps for the other) rather
    /// than stack.
    drawer: Option(domain.Drawer),
    /// Currently open bottom sheet on phone. Also drives the desktop
    /// right-rail (DetailsSheet → details_panel) and floating voice
    /// popover (VoiceSheet → voice_popover.view).
    sheet: Option(domain.Sheet),
    /// Receipts received per outgoing message, keyed by message id
    /// (value_hash hex). Each entry is the set of peer verifying-key
    /// short-hex strings that have acknowledged. The bridge filters
    /// self-receipts at the source so this dict never contains them.
    receipts: Dict(String, Set(String)),
    /// Message currently "selected" — its action toolbar (react / info)
    /// stays visible. On mobile (no hover) this is the only way to
    /// reveal those buttons; on desktop it pins the row even after the
    /// pointer leaves. Tap/click anywhere on the message body toggles.
    selected_msg_id: Option(String),
    /// Currently-open peer status popover, if any. `Some(member_id)`
    /// when open, `None` when closed.
    peer_status_popover: Option(domain.MemberId),
    /// Wall-clock unix-ms snapshot. Updated every second by the
    /// `Tick(now_ms)` message so the popover's age readout stays live.
    now_ms: Int,
    /// Set of spoiler keys whose content is currently visible.
    /// Each key is `#(message_id, offset)` where offset is the
    /// character position of the `||` in the original message text.
    /// Reset to empty whenever the user navigates to a different room.
    revealed_spoilers: set.Set(#(String, Int)),
  )
}

pub type Msg {
  NoOp
  ToggleMode
  HashChanged(String)
  UpdateLandingInput(String)
  UpdateSidebarSearch(String)
  JoinRoom(String)
  DeleteRoom(String)
  GoToLanding
  DragRoomStart(String)
  DragRoomOver(String)
  DragRoomLeave(String)
  DropRoomOn(String)
  DragRoomEnd
  SelectChannel(ChannelId)
  ToggleRoomsRail
  UpdateDraft(String)
  ToggleMessageSelected(String)
  ToggleReactionPicker(String)
  ReactionsChanged(target: String, snapshot: Dict(String, Set(String)))
  ToggleReactionEmoji(target: String, emoji: String)
  ReactionSent(Result(Nil, String))
  OpenFullEmojiPicker(String)
  CloseFullEmojiPicker
  OpenDetail(String)
  CloseDetail
  OpenVoicePopover(String)
  CloseVoicePopover
  OpenPeerStatusPopover(domain.MemberId)
  ClosePeerStatusPopover
  Tick(Int)
  SetMemberVolume(String, Int)
  ToggleMemberDenoise(String)
  ToggleMemberDeafen(String)
  ResetMemberVoice(String)
  // sunset-web-wasm bridge wiring:
  IdentityReady(BitArray)
  ClientReady(ClientHandle)
  RelayConnectResult(Result(Nil, String))
  SubscribePublishResult(Result(Nil, String))
  IncomingMsg(IncomingMessage)
  IncomingReceipt(message_id: String, from_pubkey: String)
  SubmitDraft
  MessageSent(Result(String, String))
  MembersUpdated(List(domain.Member))
  RelayStatusUpdated(String)
  ViewportChanged(domain.Viewport)
  OpenDrawer(domain.Drawer)
  CloseDrawer
  ToggleSpoiler(message_id: String, offset: Int)
  ApplyComposerShortcut(
    before: String,
    between: String,
    after: String,
    caret_at_between: Bool,
  )
}

pub fn main() {
  storage.install_mobile_viewport_meta()
  scroll_anchor.attach_chat_scroll_anchor()
  let app = lustre.application(init, update, view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}

fn init(_flags: Nil) -> #(Model, Effect(Msg)) {
  let stored_rooms = storage.read_joined_rooms()
  let initial_hash = storage.read_hash()
  let initial_mode = case storage.read_saved_theme() {
    "dark" -> Dark
    "light" -> Light
    // No explicit choice yet: follow the OS / browser preference.
    _ ->
      case storage.prefers_dark() {
        True -> Dark
        False -> Light
      }
  }

  // Resolve the initial view + rooms list:
  //   * URL fragment wins if present.
  //   * Otherwise fall back to the first joined room (the user's
  //     stored ordering — head is the default).
  //   * Otherwise show the landing page.
  // Direct-navigation to a room not yet in the joined list auto-adds
  // it at the top, so a shared link behaves like a fresh "join new
  // room" rather than dropping the user back to landing.
  let initial_view = case initial_hash, stored_rooms {
    "", [] -> LandingView
    "", [first, ..] -> RoomView(first)
    name, _ -> RoomView(name)
  }

  let joined = case initial_view {
    LandingView -> stored_rooms
    RoomView(name) -> ensure_joined(stored_rooms, name)
  }

  let initial_viewport = case storage.is_phone_viewport() {
    True -> domain.Phone
    False -> domain.Desktop
  }

  let model =
    Model(
      mode: initial_mode,
      view: initial_view,
      joined_rooms: joined,
      rooms_collapsed: False,
      landing_input: "",
      sidebar_search: "",
      current_channel: ChannelId(fixture.initial_channel_id),
      draft: "",
      reacting_to: None,
      full_picker_for: None,
      reactions: dict.new(),
      dragging_room: None,
      drag_over_room: None,
      voice_settings: seed_voice_settings(),
      client: None,
      messages: [],
      relay_status: "disconnected",
      members: [],
      viewport: initial_viewport,
      drawer: None,
      sheet: None,
      receipts: dict.new(),
      selected_msg_id: None,
      peer_status_popover: None,
      now_ms: 0,
      revealed_spoilers: set.new(),
    )

  let subscribe_hash =
    effect.from(fn(dispatch) {
      storage.on_hash_change(fn(hash) { dispatch(HashChanged(hash)) })
    })

  let initial_persist = case joined == stored_rooms {
    True -> effect.none()
    False ->
      effect.from(fn(_) {
        storage.write_joined_rooms(joined)
        Nil
      })
  }

  let initial_hash_sync = case initial_view, initial_hash {
    RoomView(name), "" ->
      effect.from(fn(_) {
        storage.set_hash(name)
        Nil
      })
    _, _ -> effect.none()
  }

  // Bootstrap the sunset-web-wasm bridge: load identity → create client →
  // (optionally) connect to relay from ?relay=… query param.
  let bootstrap =
    effect.from(fn(dispatch) {
      sunset.load_or_create_identity(fn(seed) { dispatch(IdentityReady(seed)) })
    })

  let subscribe_viewport =
    effect.from(fn(dispatch) {
      storage.on_viewport_change(fn(is_phone) {
        let v = case is_phone {
          True -> domain.Phone
          False -> domain.Desktop
        }
        dispatch(ViewportChanged(v))
      })
    })

  let subscribe_touch_drag =
    effect.from(fn(dispatch) {
      touch_drag.attach(
        touch_drag.Callbacks(
          on_start: fn(name) { dispatch(DragRoomStart(name)) },
          on_over: fn(name) { dispatch(DragRoomOver(name)) },
          on_drop: fn(name) { dispatch(DropRoomOn(name)) },
          on_end: fn() { dispatch(DragRoomEnd) },
        ),
      )
    })

  let ticker_eff =
    effect.from(fn(dispatch) {
      sunset.set_interval_ms(1000, fn() { dispatch(Tick(sunset.now_ms())) })
    })

  let attach_shortcuts_eff =
    effect.from(fn(_dispatch) {
      composer.attach_shortcut_prevent_default("composer-textarea")
    })

  #(
    model,
    effect.batch([
      subscribe_hash,
      initial_persist,
      initial_hash_sync,
      bootstrap,
      subscribe_viewport,
      subscribe_touch_drag,
      ticker_eff,
      attach_shortcuts_eff,
    ]),
  )
}

/// Seed per-member voice tweaks for every in-call member: full
/// volume, denoise on, not locally muted. Keyed by member name so
/// callers don't need to thread MemberId through the popover wiring.
fn seed_voice_settings() -> Dict(String, domain.VoiceSettings) {
  fixture.members()
  |> list.fold(dict.new(), fn(d, m) {
    case m.in_call {
      True ->
        dict.insert(
          d,
          m.name,
          domain.VoiceSettings(volume: 100, denoise: True, deafened: False),
        )
      False -> d
    }
  })
}

fn member_voice_settings(
  settings: Dict(String, domain.VoiceSettings),
  name: String,
) -> domain.VoiceSettings {
  case dict.get(settings, name) {
    Ok(s) -> s
    Error(_) ->
      domain.VoiceSettings(volume: 100, denoise: True, deafened: False)
  }
}

/// Add `name` to `existing` if it isn't already present (prepending
/// at the head, which is where new rooms appear). If `name` is
/// already in the list it is returned unchanged — selecting an
/// existing room must NOT reorder the rail; only an explicit
/// drag-drop reorders the user-managed list.
fn ensure_joined(existing: List(String), name: String) -> List(String) {
  case list.contains(existing, name) {
    True -> existing
    False -> [name, ..existing]
  }
}

/// Move `name` so it lands immediately before `target`. If `target`
/// is the same as `name` (drop on self) the list is returned
/// unchanged. If `name` is already adjacent above `target` the move
/// is also a no-op.
fn reorder_before(
  rooms: List(String),
  name: String,
  target: String,
) -> List(String) {
  case name == target {
    True -> rooms
    False -> {
      let without = list.filter(rooms, fn(r) { r != name })
      list.flatten(
        list.map(without, fn(r) {
          case r == target {
            True -> [name, r]
            False -> [r]
          }
        }),
      )
    }
  }
}

fn sanitize(raw: String) -> String {
  string.trim(raw)
}

@external(javascript, "./sunset_web/sunset.ffi.mjs", "currentTimeMs")
fn current_time_ms() -> Int

@external(javascript, "./sunset_web/sunset.ffi.mjs", "shortPubkey")
fn short_pubkey(bits: BitArray) -> String

@external(javascript, "./sunset_web/sunset.ffi.mjs", "shortInitials")
fn short_initials(bits: BitArray) -> String

@external(javascript, "./sunset_web/sunset.ffi.mjs", "formatTimeMs")
fn format_time_ms(ms: Int) -> String

fn update(model: Model, msg: Msg) -> #(Model, Effect(Msg)) {
  case msg {
    NoOp -> #(model, effect.none())
    ToggleMode -> {
      let next_mode = case model.mode {
        Light -> Dark
        Dark -> Light
      }
      let label = case next_mode {
        Light -> "light"
        Dark -> "dark"
      }
      #(
        Model(..model, mode: next_mode),
        effect.from(fn(_) {
          storage.write_saved_theme(label)
          Nil
        }),
      )
    }
    HashChanged(hash) -> {
      let new_view = case hash {
        "" -> LandingView
        name -> RoomView(name)
      }
      let new_rooms = case new_view {
        LandingView -> model.joined_rooms
        RoomView(name) -> ensure_joined(model.joined_rooms, name)
      }
      let persisted = case new_rooms == model.joined_rooms {
        True -> effect.none()
        False ->
          effect.from(fn(_) {
            storage.write_joined_rooms(new_rooms)
            Nil
          })
      }
      #(
        Model(
          ..model,
          view: new_view,
          joined_rooms: new_rooms,
          revealed_spoilers: set.new(),
        ),
        persisted,
      )
    }
    UpdateLandingInput(s) -> #(Model(..model, landing_input: s), effect.none())
    UpdateSidebarSearch(s) -> #(
      Model(..model, sidebar_search: s),
      effect.none(),
    )
    JoinRoom(raw) -> {
      let name = sanitize(raw)
      case name {
        "" -> #(model, effect.none())
        _ -> {
          let was_new = !list.contains(model.joined_rooms, name)
          let new_rooms = ensure_joined(model.joined_rooms, name)
          // On phone, picking a room from the rooms drawer should land
          // the user in the channels drawer for the new room (so they
          // can pick a channel). Otherwise close to chat as before.
          let new_drawer = case model.viewport, model.drawer {
            domain.Phone, Some(domain.RoomsDrawer) ->
              Some(domain.ChannelsDrawer)
            _, _ -> None
          }
          let new_model =
            Model(
              ..model,
              joined_rooms: new_rooms,
              view: RoomView(name),
              landing_input: "",
              sidebar_search: "",
              drawer: new_drawer,
              revealed_spoilers: set.new(),
            )
          let persist_eff = case was_new {
            True ->
              effect.from(fn(_) {
                storage.write_joined_rooms(new_rooms)
                Nil
              })
            False -> effect.none()
          }
          #(
            new_model,
            effect.batch([
              persist_eff,
              effect.from(fn(_) {
                storage.set_hash(name)
                Nil
              }),
            ]),
          )
        }
      }
    }
    DeleteRoom(name) -> {
      let new_rooms = list.filter(model.joined_rooms, fn(r) { r != name })
      let active_was_deleted = case model.view {
        RoomView(active) -> active == name
        LandingView -> False
      }
      let new_view = case active_was_deleted, new_rooms {
        True, [next, ..] -> RoomView(next)
        True, [] -> LandingView
        False, _ -> model.view
      }
      let persist =
        effect.from(fn(_) {
          storage.write_joined_rooms(new_rooms)
          case new_view {
            RoomView(n) -> storage.set_hash(n)
            LandingView -> storage.set_hash("")
          }
          Nil
        })
      #(Model(..model, joined_rooms: new_rooms, view: new_view), persist)
    }
    GoToLanding -> {
      let persist =
        effect.from(fn(_) {
          storage.set_hash("")
          Nil
        })
      #(Model(..model, view: LandingView), persist)
    }
    DragRoomStart(name) -> #(
      Model(..model, dragging_room: Some(name)),
      effect.none(),
    )
    DragRoomOver(name) -> {
      let next = case model.drag_over_room {
        Some(current) if current == name -> model.drag_over_room
        _ -> Some(name)
      }
      #(Model(..model, drag_over_room: next), effect.none())
    }
    DragRoomLeave(name) -> {
      let next = case model.drag_over_room {
        Some(current) if current == name -> None
        _ -> model.drag_over_room
      }
      #(Model(..model, drag_over_room: next), effect.none())
    }
    DropRoomOn(target) -> {
      case model.dragging_room {
        None -> #(Model(..model, drag_over_room: None), effect.none())
        Some(src) -> {
          let new_rooms = reorder_before(model.joined_rooms, src, target)
          let persist = case new_rooms == model.joined_rooms {
            True -> effect.none()
            False ->
              effect.from(fn(_) {
                storage.write_joined_rooms(new_rooms)
                Nil
              })
          }
          #(
            Model(
              ..model,
              joined_rooms: new_rooms,
              dragging_room: None,
              drag_over_room: None,
            ),
            persist,
          )
        }
      }
    }
    DragRoomEnd -> #(
      Model(..model, dragging_room: None, drag_over_room: None),
      effect.none(),
    )
    SelectChannel(id) -> #(Model(..model, current_channel: id), effect.none())
    ToggleRoomsRail -> #(
      Model(..model, rooms_collapsed: !model.rooms_collapsed),
      effect.none(),
    )
    UpdateDraft(s) -> #(
      Model(..model, draft: s),
      effect.from(fn(_dispatch) { composer.auto_grow("composer-textarea") }),
    )
    IdentityReady(seed) -> {
      let create_client_eff =
        effect.from(fn(dispatch) {
          sunset.create_client(seed, "sunset-demo", fn(client) {
            dispatch(ClientReady(client))
          })
        })
      #(model, create_client_eff)
    }
    ClientReady(client) -> {
      let on_msg_eff =
        effect.from(fn(dispatch) {
          sunset.on_message(client, fn(im) { dispatch(IncomingMsg(im)) })
        })
      let on_receipt_eff =
        effect.from(fn(dispatch) {
          sunset.on_receipt(client, fn(r) {
            dispatch(IncomingReceipt(
              sunset.rec_for_value_hash_hex(r),
              short_pubkey(sunset.rec_from_pubkey(r)),
            ))
          })
        })
      // Presence wiring is in ClientReady (not RelayConnectResult) so
      // it kicks off even when there's no `?relay=` URL — the user
      // still sees themselves in the member list. Effect order within a
      // batch is unspecified by Lustre, but that's fine: Client::start_presence
      // snapshots the engine's current peer set after subscribing, so
      // already-connected peers are picked up regardless of when
      // start_presence runs relative to add_relay.
      let presence_eff =
        effect.from(fn(dispatch) {
          let #(interval, ttl, refresh) = sunset.presence_params_from_url()
          sunset.start_presence(client, interval, ttl, refresh)
          sunset.on_members_changed(client, fn(ms) {
            dispatch(MembersUpdated(map_members(ms)))
          })
          sunset.on_relay_status_changed(client, fn(s) {
            dispatch(RelayStatusUpdated(s))
          })
        })
      // Default relays the client dials when no `?relay=…` query
      // parameter is supplied. The query param, when present, replaces
      // (does not extend) this list — pasting an explicit relay opts
      // out of the defaults.
      let relays = case sunset.relay_url_param() {
        Ok(url) -> [url]
        Error(_) -> default_relays
      }
      let connect_eff =
        effect.from(fn(dispatch) {
          list.each(relays, fn(url) {
            sunset.add_relay(client, url, fn(r) {
              dispatch(RelayConnectResult(r))
            })
          })
        })
      let on_reactions_eff =
        effect.from(fn(dispatch) {
          sunset.on_reactions_changed(client, fn(snapshot_payload) {
            let target = sunset.reactions_snapshot_target_hex(snapshot_payload)
            let entries = sunset.reactions_snapshot_entries(snapshot_payload)
            let inner_dict =
              list.fold(entries, dict.new(), fn(d, pair) {
                let #(emoji, authors) = pair
                dict.insert(d, emoji, set.from_list(authors))
              })
            dispatch(ReactionsChanged(target, inner_dict))
          })
        })
      let new_status = case relays {
        [] -> "disconnected"
        _ -> "connecting"
      }
      #(
        Model(..model, client: Some(client), relay_status: new_status),
        effect.batch([
          on_receipt_eff,
          on_msg_eff,
          presence_eff,
          on_reactions_eff,
          connect_eff,
        ]),
      )
    }
    RelayConnectResult(Ok(_)) ->
      case model.client {
        Some(client) -> {
          let pub_eff =
            effect.from(fn(dispatch) {
              sunset.publish_room_subscription(client, fn(r) {
                dispatch(SubscribePublishResult(r))
              })
            })
          #(Model(..model, relay_status: "connected"), pub_eff)
        }
        None -> #(model, effect.none())
      }
    RelayConnectResult(Error(_)) -> #(
      Model(..model, relay_status: "error"),
      effect.none(),
    )
    SubscribePublishResult(_) -> #(model, effect.none())
    IncomingMsg(im) -> {
      let new_msg =
        domain.Message(
          id: sunset.inc_value_hash_hex(im),
          author: short_pubkey(sunset.inc_author_pubkey(im)),
          initials: short_initials(sunset.inc_author_pubkey(im)),
          time: format_time_ms(sunset.inc_sent_at_ms(im)),
          body: sunset.inc_body(im),
          seen_by: 0,
          you: sunset.inc_is_self(im),
          pending: False,
          reactions: [],
          bridge: NoBridge,
          details: domain.NoDetails,
        )
      // Append; dedupe by id to handle Replay::All re-emits.
      let updated = case
        list.any(model.messages, fn(m) { m.id == new_msg.id })
      {
        True -> model.messages
        False -> list.append(model.messages, [new_msg])
      }
      #(Model(..model, messages: updated), effect.none())
    }
    SubmitDraft -> {
      let body = sanitize(model.draft)
      case body, model.client {
        "", _ -> #(model, effect.none())
        _, None -> #(model, effect.none())
        body, Some(client) -> {
          let send_eff =
            effect.from(fn(dispatch) {
              sunset.send_message(client, body, current_time_ms(), fn(r) {
                dispatch(MessageSent(r))
              })
            })
          #(Model(..model, draft: ""), send_eff)
        }
      }
    }
    MessageSent(_) -> #(model, effect.none())
    IncomingReceipt(message_id, from_pubkey) -> {
      let existing = case dict.get(model.receipts, message_id) {
        Ok(s) -> s
        Error(_) -> set.new()
      }
      let updated = set.insert(existing, from_pubkey)
      #(
        Model(
          ..model,
          receipts: dict.insert(model.receipts, message_id, updated),
        ),
        effect.none(),
      )
    }
    MembersUpdated(ms) -> {
      // If the open popover's target left, close it.
      let next_popover = case model.peer_status_popover {
        None -> None
        Some(target) ->
          case list.find(ms, fn(m) { m.id == target }) {
            Ok(_) -> Some(target)
            Error(_) -> None
          }
      }
      #(
        Model(..model, members: ms, peer_status_popover: next_popover),
        effect.none(),
      )
    }
    RelayStatusUpdated(s) -> #(Model(..model, relay_status: s), effect.none())
    ToggleMessageSelected(id) -> {
      // Tap/click on a message body. Toggle selection — same id
      // deselects, different id replaces. Closing also dismisses any
      // open reaction picker for the previously-selected message so
      // the UI doesn't end up with a phantom picker on a hidden row.
      let next = case model.selected_msg_id {
        Some(open) if open == id -> None
        _ -> Some(id)
      }
      let next_picker = case next {
        None -> None
        Some(_) -> model.reacting_to
      }
      #(
        Model(..model, selected_msg_id: next, reacting_to: next_picker),
        effect.none(),
      )
    }
    ToggleReactionPicker(id) -> {
      let next = case model.reacting_to {
        Some(open) if open == id -> None
        _ -> Some(id)
      }
      #(Model(..model, reacting_to: next), effect.none())
    }
    OpenFullEmojiPicker(target) -> {
      // Trigger the lazy import so the web component is registered by the
      // time we mount it.
      sunset.register_emoji_picker()
      #(
        Model(..model, full_picker_for: Some(target), reacting_to: None),
        effect.none(),
      )
    }
    CloseFullEmojiPicker -> #(
      Model(..model, full_picker_for: None),
      effect.none(),
    )
    ReactionsChanged(target, snapshot) -> {
      #(
        Model(
          ..model,
          reactions: dict.insert(model.reactions, target, snapshot),
        ),
        effect.none(),
      )
    }
    ToggleReactionEmoji(target, emoji) -> {
      let self_pubkey_hex_opt =
        option.map(model.client, fn(c) { client_pubkey_hex(c) })
      let action = case dict.get(model.reactions, target) {
        Ok(snap) ->
          case dict.get(snap, emoji), self_pubkey_hex_opt {
            Ok(authors), Some(me) ->
              case set.contains(authors, me) {
                True -> "remove"
                False -> "add"
              }
            _, _ -> "add"
          }
        Error(_) -> "add"
      }
      let next_model = Model(..model, reacting_to: None, full_picker_for: None)
      let send_effect = case model.client {
        Some(c) ->
          effect.from(fn(dispatch) {
            let now = sunset.now_ms()
            sunset.send_reaction(c, target, emoji, action, now, fn(result) {
              dispatch(ReactionSent(result))
            })
          })
        None -> effect.none()
      }
      #(next_model, send_effect)
    }
    ReactionSent(Ok(_)) -> #(model, effect.none())
    ReactionSent(Error(msg)) -> {
      io.println("send_reaction error: " <> msg)
      #(model, effect.none())
    }
    OpenDetail(id) -> #(
      // Opening the details panel pins selection on the same id so the
      // row's action toolbar stays visible while the panel is open and
      // no other row's hover affordance can sneak in alongside it
      // (the global stylesheet's :has() rule keys off .is-selected).
      Model(
        ..model,
        sheet: Some(domain.DetailsSheet(message_id: id)),
        reacting_to: None,
        selected_msg_id: Some(id),
      ),
      effect.none(),
    )
    CloseDetail -> {
      // Only close if the active sheet is the details one — guards against
      // a Voice sheet being opened concurrently and accidentally dismissed.
      // When closing, drop the matching selection so the toolbar collapses
      // back to its default state.
      let #(next_sheet, next_selected) = case model.sheet {
        Some(domain.DetailsSheet(message_id: id)) -> #(
          None,
          case model.selected_msg_id {
            Some(open) if open == id -> None
            other -> other
          },
        )
        other -> #(other, model.selected_msg_id)
      }
      #(
        Model(..model, sheet: next_sheet, selected_msg_id: next_selected),
        effect.none(),
      )
    }
    OpenVoicePopover(name) -> #(
      Model(
        ..model,
        sheet: Some(domain.VoiceSheet(member_name: name)),
        reacting_to: None,
      ),
      effect.none(),
    )
    CloseVoicePopover -> #(
      Model(..model, sheet: case model.sheet {
        Some(domain.VoiceSheet(_)) -> None
        other -> other
      }),
      effect.none(),
    )
    OpenPeerStatusPopover(member_id) -> #(
      Model(..model, peer_status_popover: Some(member_id)),
      effect.none(),
    )
    ClosePeerStatusPopover -> #(
      Model(..model, peer_status_popover: None),
      effect.none(),
    )
    Tick(now) -> #(Model(..model, now_ms: now), effect.none())
    SetMemberVolume(name, value) -> {
      let settings = member_voice_settings(model.voice_settings, name)
      let next = domain.VoiceSettings(..settings, volume: value)
      #(
        Model(
          ..model,
          voice_settings: dict.insert(model.voice_settings, name, next),
        ),
        effect.none(),
      )
    }
    ToggleMemberDenoise(name) -> {
      let settings = member_voice_settings(model.voice_settings, name)
      let next = domain.VoiceSettings(..settings, denoise: !settings.denoise)
      #(
        Model(
          ..model,
          voice_settings: dict.insert(model.voice_settings, name, next),
        ),
        effect.none(),
      )
    }
    ToggleMemberDeafen(name) -> {
      let settings = member_voice_settings(model.voice_settings, name)
      let next = domain.VoiceSettings(..settings, deafened: !settings.deafened)
      #(
        Model(
          ..model,
          voice_settings: dict.insert(model.voice_settings, name, next),
        ),
        effect.none(),
      )
    }
    ResetMemberVoice(name) -> #(
      Model(
        ..model,
        voice_settings: dict.insert(
          model.voice_settings,
          name,
          domain.VoiceSettings(volume: 100, denoise: True, deafened: False),
        ),
      ),
      effect.none(),
    )
    ViewportChanged(v) -> {
      // Crossing the boundary in either direction closes any open drawer.
      // Sheets intentionally survive: DetailsSheet and VoiceSheet render
      // on both viewports (right-rail / floating popover on desktop).
      #(Model(..model, viewport: v, drawer: None), effect.none())
    }
    OpenDrawer(d) -> #(Model(..model, drawer: Some(d)), effect.none())
    CloseDrawer -> #(Model(..model, drawer: None), effect.none())
    ToggleSpoiler(mid, off) -> {
      let key = #(mid, off)
      let next = case set.contains(model.revealed_spoilers, key) {
        True -> set.delete(model.revealed_spoilers, key)
        False -> set.insert(model.revealed_spoilers, key)
      }
      #(Model(..model, revealed_spoilers: next), effect.none())
    }
    ApplyComposerShortcut(b, m, a, caret) -> {
      let new_value =
        composer.apply_template("composer-textarea", b, m, a, caret)
      #(Model(..model, draft: new_value), effect.none())
    }
  }
}

fn view(model: Model) -> Element(Msg) {
  let palette = theme.palette_for(model.mode)
  case model.view {
    LandingView ->
      landing.view(
        palette: palette,
        mode: model.mode,
        viewport: model.viewport,
        input: model.landing_input,
        noop: NoOp,
        on_input: UpdateLandingInput,
        on_join: JoinRoom,
        on_toggle_mode: ToggleMode,
      )
    RoomView(name) -> room_view(model, palette, name)
  }
}

fn room_view(model: Model, palette, current_name: String) -> Element(Msg) {
  let displayed_rooms = resolve_rooms(model.joined_rooms, model.relay_status)
  let filtered = filter_rooms(displayed_rooms, model.sidebar_search)
  let active_room =
    lookup_room(displayed_rooms, current_name, model.relay_status)

  let self_pubkey_hex = option.map(model.client, fn(c) { client_pubkey_hex(c) })
  let raw_messages = model.messages
  let messages_with_live_reactions =
    list.map(raw_messages, fn(m) {
      case dict.get(model.reactions, m.id) {
        Ok(snap) ->
          domain.Message(
            ..m,
            reactions: snapshot_to_reactions(snap, self_pubkey_hex),
          )
        Error(_) -> m
      }
    })

  let detail_msg = case model.sheet {
    Some(domain.DetailsSheet(message_id: id)) ->
      find_message(messages_with_live_reactions, id)
    _ -> None
  }

  let reaction_sheet_el = case model.viewport, model.reacting_to {
    domain.Phone, Some(id) ->
      bottom_sheet.view(
        palette: palette,
        open: True,
        on_close: ToggleReactionPicker(id),
        test_id: "reaction-sheet",
        content: phone_reaction_grid(palette, id),
      )
    _, _ -> element.fragment([])
  }

  // Desktop: centered fixed-position overlay (goes in the shell's overlay slot).
  let full_picker_overlay_el = case model.full_picker_for {
    Some(target) ->
      html.div(
        [
          attribute.attribute("data-testid", "full-emoji-picker-overlay"),
          ui.css([
            #("position", "fixed"),
            #("top", "50%"),
            #("left", "50%"),
            #("transform", "translate(-50%, -50%)"),
            #("z-index", "100"),
            #("background", palette.surface),
            #("border", "1px solid " <> palette.border),
            #("border-radius", "8px"),
            #("box-shadow", palette.shadow_lg),
          ]),
        ],
        [
          emoji_picker.view(fn(emoji) { ToggleReactionEmoji(target, emoji) }),
        ],
      )
    None -> element.fragment([])
  }

  // Phone: bottom sheet (merged into the reaction_sheet slot).
  let full_picker_sheet_el = case model.full_picker_for {
    Some(target) ->
      bottom_sheet.view(
        palette: palette,
        open: True,
        on_close: CloseFullEmojiPicker,
        test_id: "full-emoji-picker-sheet",
        content: emoji_picker.view(fn(emoji) {
          ToggleReactionEmoji(target, emoji)
        }),
      )
    None -> element.fragment([])
  }

  let details_sheet_el = case model.viewport, model.sheet {
    domain.Phone, Some(domain.DetailsSheet(message_id: id)) ->
      case find_message(messages_with_live_reactions, id) {
        Some(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: CloseDetail,
            test_id: "details-sheet",
            content: details_panel.view(
              palette: palette,
              message: m,
              receipts: receipts_for(model.receipts, m.id),
              members: model.members,
              on_close: CloseDetail,
            ),
          )
        None -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let voice_sheet_el = case model.viewport, model.sheet {
    domain.Phone, Some(domain.VoiceSheet(member_name: name)) ->
      case list.find(fixture.members(), fn(m) { m.name == name }) {
        Ok(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: CloseVoicePopover,
            test_id: "voice-sheet",
            content: voice_popover.view(
              palette: palette,
              placement: voice_popover.InSheet,
              member: m,
              settings: member_voice_settings(model.voice_settings, name),
              on_close: CloseVoicePopover,
              on_set_volume: fn(v) { SetMemberVolume(name, v) },
              on_toggle_denoise: ToggleMemberDenoise(name),
              on_toggle_deafen: ToggleMemberDeafen(name),
              on_reset: ResetMemberVoice(name),
            ),
          )
        Error(_) -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let peer_status_sheet_el = case model.viewport, model.peer_status_popover {
    domain.Phone, Some(member_id) ->
      case list.find(model.members, fn(m) { m.id == member_id }) {
        Ok(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: ClosePeerStatusPopover,
            test_id: "peer-status-sheet",
            content: peer_status_popover.view(
              palette: palette,
              member: m,
              now_ms: model.now_ms,
              placement: peer_status_popover.InSheet,
              on_close: ClosePeerStatusPopover,
            ),
          )
        Error(_) -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let user_in_call = list.any(fixture.members(), fn(m) { m.you && m.in_call })

  let active_voice_channel_name =
    list.find(fixture.channels(), fn(c) {
      c.kind == domain.Voice && c.in_call > 0
    })
    |> result.map(fn(c) { c.name })
    |> result.unwrap("")

  let voice_minibar_el = case model.viewport, user_in_call {
    domain.Phone, True ->
      voice_minibar.view(
        palette: palette,
        channel_name: active_voice_channel_name,
        on_open: OpenVoicePopover("you"),
      )
    _, _ -> element.fragment([])
  }

  shell.view(
    model.mode,
    palette,
    model.viewport,
    model.rooms_collapsed,
    detail_msg != None,
    model.drawer,
    ToggleMode,
    CloseDrawer,
    rooms.view(
      palette: palette,
      rooms: filtered,
      current_room: RoomId(current_name),
      collapsed: model.rooms_collapsed,
      search: model.sidebar_search,
      noop: NoOp,
      dragging: model.dragging_room,
      drag_over: model.drag_over_room,
      on_select_room: fn(id) {
        let RoomId(name) = id
        JoinRoom(name)
      },
      on_search_change: UpdateSidebarSearch,
      on_join: JoinRoom,
      on_delete: DeleteRoom,
      on_drag_start: DragRoomStart,
      on_drag_over: DragRoomOver,
      on_drag_leave: DragRoomLeave,
      on_drop: DropRoomOn,
      on_drag_end: DragRoomEnd,
      toggle: ToggleRoomsRail,
      viewport: model.viewport,
      members: model.members,
      mode: model.mode,
      on_toggle_mode: ToggleMode,
    ),
    // Voice path stays fixture-backed (in-call counts) — real voice presence is V3.
    channels.view(
      palette: palette,
      room: active_room,
      channels: fixture.channels(),
      members: fixture.members(),
      current_channel: model.current_channel,
      voice_popover_open: case model.sheet {
        Some(domain.VoiceSheet(member_name: name)) -> Some(name)
        _ -> None
      },
      on_select_channel: SelectChannel,
      on_open_voice_popover: OpenVoicePopover,
      viewport: model.viewport,
      on_open_rooms: OpenDrawer(domain.RoomsDrawer),
    ),
    main_panel.view(
      palette: palette,
      viewport: model.viewport,
      current_channel: model.current_channel,
      messages: messages_with_live_reactions,
      draft: model.draft,
      on_draft: UpdateDraft,
      on_submit: SubmitDraft,
      noop: NoOp,
      on_shortcut: fn(b, m, a, caret) { ApplyComposerShortcut(b, m, a, caret) },
      reacting_to: model.reacting_to,
      detail_msg_id: case model.sheet {
        Some(domain.DetailsSheet(message_id: id)) -> Some(id)
        _ -> None
      },
      on_toggle_reaction_picker: ToggleReactionPicker,
      on_add_reaction: ToggleReactionEmoji,
      on_open_full_picker: OpenFullEmojiPicker,
      on_open_detail: OpenDetail,
      receipts: model.receipts,
      selected_msg_id: model.selected_msg_id,
      on_toggle_selected: ToggleMessageSelected,
      is_spoiler_revealed: fn(k: markdown.SpoilerKey) {
        set.contains(model.revealed_spoilers, #(k.message_id, k.offset))
      },
      on_toggle_spoiler: fn(k: markdown.SpoilerKey) {
        ToggleSpoiler(k.message_id, k.offset)
      },
      voice_minibar: voice_minibar_el,
    ),
    case model.viewport, detail_msg {
      domain.Desktop, Some(m) ->
        details_panel.view(
          palette: palette,
          message: m,
          receipts: receipts_for(model.receipts, m.id),
          members: model.members,
          on_close: CloseDetail,
        )
      _, _ ->
        members.view(
          palette: palette,
          members: model.members,
          on_open_status: OpenPeerStatusPopover,
        )
    },
    element.fragment([
      voice_popover_overlay(palette, model),
      peer_status_popover_overlay(palette, model),
      full_picker_overlay_el,
    ]),
    phone_header.view(
      palette: palette,
      room: active_room,
      on_open_channels: OpenDrawer(domain.ChannelsDrawer),
      on_open_members: OpenDrawer(domain.MembersDrawer),
    ),
    details_sheet_el,
    voice_sheet_el,
    peer_status_sheet_el,
    element.fragment([reaction_sheet_el, full_picker_sheet_el]),
  )
}

fn voice_popover_overlay(palette, model: Model) -> Element(Msg) {
  case model.viewport, model.sheet {
    domain.Desktop, Some(domain.VoiceSheet(member_name: name)) ->
      // Voice path stays fixture-backed (in-call counts) — real voice presence is V3.
      case list.find(fixture.members(), fn(m) { m.name == name }) {
        Error(_) -> element.fragment([])
        Ok(m) ->
          voice_popover.view(
            palette: palette,
            placement: voice_popover.Floating,
            member: m,
            settings: member_voice_settings(model.voice_settings, name),
            on_close: CloseVoicePopover,
            on_set_volume: fn(v) { SetMemberVolume(name, v) },
            on_toggle_denoise: ToggleMemberDenoise(name),
            on_toggle_deafen: ToggleMemberDeafen(name),
            on_reset: ResetMemberVoice(name),
          )
      }
    _, _ -> element.fragment([])
  }
}

/// Floating popover for desktop. The phone path renders through
/// `peer_status_sheet_el` (a `bottom_sheet.view` wrapper), matching the
/// voice popover convention.
fn peer_status_popover_overlay(palette, model: Model) -> Element(Msg) {
  case model.viewport, model.peer_status_popover {
    domain.Desktop, Some(member_id) ->
      case list.find(model.members, fn(m) { m.id == member_id }) {
        Error(_) -> element.fragment([])
        Ok(m) ->
          peer_status_popover.view(
            palette: palette,
            member: m,
            now_ms: model.now_ms,
            placement: peer_status_popover.Floating,
            on_close: ClosePeerStatusPopover,
          )
      }
    _, _ -> element.fragment([])
  }
}

fn filter_rooms(rs: List(Room), search: String) -> List(Room) {
  let needle = string.lowercase(string.trim(search))
  case needle {
    "" -> rs
    _ ->
      list.filter(rs, fn(r) {
        string.contains(does: string.lowercase(r.name), contain: needle)
      })
  }
}

/// Resolve a list of joined room names to rich Room records. Names
/// that match a fixture room reuse its mock data; anything else falls
/// back to a synthetic Room so the rail still renders something useful.
fn resolve_rooms(names: List(String), relay_status: String) -> List(Room) {
  let fixture_rooms = fixture.rooms()
  list.map(names, fn(name) {
    case list.find(fixture_rooms, fn(r) { r.name == name }) {
      Ok(r) ->
        Room(..r, status: relay_status_to_conn(relay_status), id: RoomId(name))
      Error(_) -> synthetic_room(name, relay_status)
    }
  })
}

fn lookup_room(rs: List(Room), name: String, relay_status: String) -> Room {
  case list.find(rs, fn(r) { r.name == name }) {
    Ok(r) -> r
    Error(_) -> synthetic_room(name, relay_status)
  }
}

/// Default Room record for a name we have no fixture entry for. Reads
/// like a freshly-joined room with no observed activity yet.
fn synthetic_room(name: String, relay_status: String) -> Room {
  Room(
    id: RoomId(name),
    name: name,
    members: 1,
    online: 1,
    in_call: 0,
    status: relay_status_to_conn(relay_status),
    last_active: "now",
    unread: 0,
    bridge: NoBridge,
  )
}

fn relay_status_to_conn(relay_status: String) -> domain.ConnStatus {
  case relay_status {
    "connected" -> domain.Connected
    "connecting" -> domain.Reconnecting
    "error" -> domain.Offline
    "disconnected" -> domain.Offline
    _ -> domain.Connected
  }
}

fn find_message(ms: List(Message), id: String) -> Option(Message) {
  list.find(ms, fn(m) { m.id == id })
  |> option.from_result
}

/// Convert a per-target snapshot dict into the `List(Reaction)` shape
/// the chip-row view consumes. `self_pubkey_hex` decides the
/// `by_you` flag; `None` (no client yet) treats every reaction as
/// not-by-you so the UI doesn't lie.
fn snapshot_to_reactions(
  snapshot: Dict(String, Set(String)),
  self_pubkey_hex: Option(String),
) -> List(domain.Reaction) {
  dict.to_list(snapshot)
  |> list.filter_map(fn(pair) {
    let #(emoji, authors) = pair
    case set.size(authors) {
      0 -> Error(Nil)
      n -> {
        let by_you = case self_pubkey_hex {
          Some(me) -> set.contains(authors, me)
          None -> False
        }
        Ok(domain.Reaction(emoji: emoji, count: n, by_you: by_you))
      }
    }
  })
}

fn client_pubkey_hex(c: ClientHandle) -> String {
  sunset.client_public_key_hex(c)
}

fn receipts_for(receipts: Dict(String, Set(String)), id: String) -> Set(String) {
  case dict.get(receipts, id) {
    Ok(s) -> s
    Error(_) -> set.new()
  }
}

fn map_members(ms: List(sunset.MemberJs)) -> List(domain.Member) {
  list.map(ms, fn(m) {
    let pk = sunset.mem_pubkey(m)
    domain.Member(
      id: domain.MemberId(short_pubkey(pk)),
      name: short_pubkey(pk),
      initials: short_initials(pk),
      status: presence_to_status(sunset.mem_presence(m)),
      relay: connection_mode_to_relay(sunset.mem_connection_mode(m)),
      you: sunset.mem_is_self(m),
      in_call: False,
      bridge: domain.NoBridge,
      role: domain.NoRole,
      last_heartbeat_ms: sunset.mem_last_heartbeat_ms(m),
    )
  })
}

fn presence_to_status(s: String) -> domain.Presence {
  case s {
    "online" -> domain.Online
    "away" -> domain.Away
    _ -> domain.OfflineP
  }
}

fn connection_mode_to_relay(s: String) -> domain.RelayStatus {
  case s {
    "direct" -> domain.Direct
    "via_relay" -> domain.OneHop
    "self" -> domain.SelfRelay
    _ -> domain.NoRelay
  }
}

fn phone_reaction_grid(
  palette: theme.Palette,
  message_id: String,
) -> Element(Msg) {
  let emojis = ["🌅", "👍", "👀", "🔥", "🌙"]
  html.div(
    [
      attribute.attribute("data-testid", "reaction-picker"),
      ui.css([
        #("display", "grid"),
        #("grid-template-columns", "repeat(5, 1fr)"),
        #("gap", "8px"),
        #("padding", "16px 16px 24px 16px"),
      ]),
    ],
    list.map(emojis, fn(e) {
      html.button(
        [
          attribute.attribute("aria-label", e),
          event.on_click(ToggleReactionEmoji(message_id, e)),
          ui.css([
            #("padding", "12px"),
            #("font-size", "26px"),
            #("border", "1px solid " <> palette.border_soft),
            #("background", palette.surface),
            #("color", palette.text),
            #("border-radius", "10px"),
            #("font-family", "inherit"),
            #("cursor", "pointer"),
          ]),
        ],
        [html.text(e)],
      )
    }),
  )
}
