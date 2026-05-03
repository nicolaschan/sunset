//! Reaction tracker: platform-agnostic chat-semantics layer over the
//! room's `<room_fp>/msg/` store namespace. Filters incoming entries
//! down to `MessageBody::Reaction` events, applies LWW per
//! `(author, target, emoji)` keyed on `(sent_at_ms, value_hash)`, and
//! fires whole-snapshot callbacks per affected target on debounced
//! state changes. Mirrors the shape of `crate::membership` so wasm,
//! TUI, and any future surface plug in via the same callback slot
//! pattern.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use crate::crypto::envelope::{MessageBody, ReactionAction};
use crate::identity::IdentityKey;
use sunset_store::Hash;

// MessageBody is imported here for use by spawn_reaction_tracker (B4).
#[allow(unused_imports)]
use self::MessageBody as _;

/// Per-target snapshot: emoji → set of authors currently reacting with
/// that emoji. Empty inner set means no live reactions for the emoji
/// (the emoji entry should be omitted by `derive_snapshot`).
pub type ReactionSnapshot = HashMap<String, BTreeSet<IdentityKey>>;

/// Stable signature of a snapshot used for debounce. Sorted lex on
/// emoji, then on author bytes — semantic equality, not allocation
/// identity.
pub type ReactionSig = Vec<(String, Vec<Vec<u8>>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReactionEntry {
    pub action: ReactionAction,
    pub sent_at_ms: u64,
    pub value_hash: Hash,
}

/// One decoded reaction event. Built by `spawn_reaction_tracker` from
/// each decoded `MessageBody::Reaction` and fed into `apply_event`.
#[derive(Clone, Debug)]
pub struct ReactionEvent {
    pub author: IdentityKey,
    pub target: Hash,
    pub emoji: String,
    pub action: ReactionAction,
    pub sent_at_ms: u64,
    pub value_hash: Hash,
}

/// In-memory per-tracker state. `target → emoji → author → entry`.
pub(crate) type ReactionState =
    HashMap<Hash, HashMap<String, HashMap<IdentityKey, ReactionEntry>>>;

/// Callback fired with `(target, snapshot)` whenever the snapshot for
/// `target` changes (per `reactions_signature` debounce).
pub type ReactionsCallback = Box<dyn Fn(&Hash, &ReactionSnapshot)>;

pub type ReactionsCallbackSlot = Rc<RefCell<Option<ReactionsCallback>>>;

/// Shared mutable handles between the tracker task and the host's
/// public API. Cloneable so the host (e.g. `Client`) can keep its own
/// handle alongside the spawned task's.
#[derive(Clone, Default)]
pub struct ReactionHandles {
    pub on_reactions_changed: ReactionsCallbackSlot,
    /// Per-target last-fired snapshot signature. Cleared when the host
    /// re-registers the callback so the next event refires the current
    /// state for that target.
    pub last_target_signatures: Rc<RefCell<HashMap<Hash, ReactionSig>>>,
}
