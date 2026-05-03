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

// ReactionEntry / ReactionState / apply_event / derive_snapshot are consumed
// by spawn_reaction_tracker (B4). Allow dead_code until that task lands.
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Apply one event to in-memory state. The new entry replaces an
/// existing entry for `(author, target, emoji)` iff `(sent_at_ms,
/// value_hash)` of the new entry is strictly greater than the existing
/// entry's pair. Returns `true` if the snapshot for `event.target`
/// might have changed; the caller still does a signature comparison
/// to decide whether to fire the callback (so `true` is safe to
/// over-report).
#[allow(dead_code)]
pub(crate) fn apply_event(state: &mut ReactionState, event: ReactionEvent) -> bool {
    let by_emoji = state.entry(event.target).or_default();
    let by_author = by_emoji.entry(event.emoji.clone()).or_default();
    let new_entry = ReactionEntry {
        action: event.action,
        sent_at_ms: event.sent_at_ms,
        value_hash: event.value_hash,
    };
    match by_author.get(&event.author) {
        Some(existing) => {
            let existing_key = (existing.sent_at_ms, *existing.value_hash.as_bytes());
            let new_key = (new_entry.sent_at_ms, *new_entry.value_hash.as_bytes());
            if new_key > existing_key {
                by_author.insert(event.author, new_entry);
                true
            } else {
                false
            }
        }
        None => {
            by_author.insert(event.author, new_entry);
            true
        }
    }
}

/// Render the current snapshot for one target. Authors whose latest
/// LWW entry is `Remove` are omitted; emoji entries with no remaining
/// authors are omitted.
#[allow(dead_code)]
pub(crate) fn derive_snapshot(state: &ReactionState, target: &Hash) -> ReactionSnapshot {
    let mut out = ReactionSnapshot::new();
    let Some(by_emoji) = state.get(target) else {
        return out;
    };
    for (emoji, by_author) in by_emoji {
        let mut authors = BTreeSet::new();
        for (author, entry) in by_author {
            if entry.action == ReactionAction::Add {
                authors.insert(author.clone());
            }
        }
        if !authors.is_empty() {
            out.insert(emoji.clone(), authors);
        }
    }
    out
}

/// Stable signature of a snapshot used for debounce. Sorted lex on
/// emoji, then on author key bytes. Equal snapshots produce equal
/// signatures regardless of HashMap/BTreeSet iteration order quirks.
pub fn reactions_signature(snapshot: &ReactionSnapshot) -> ReactionSig {
    let mut emoji_keys: Vec<&String> = snapshot.keys().collect();
    emoji_keys.sort();
    emoji_keys
        .into_iter()
        .map(|emoji| {
            let mut authors: Vec<Vec<u8>> = snapshot[emoji]
                .iter()
                .map(|k| k.as_bytes().to_vec())
                .collect();
            authors.sort();
            (emoji.clone(), authors)
        })
        .collect()
}

#[cfg(test)]
mod apply_event_tests {
    use super::*;
    use rand_core::OsRng;

    fn alice() -> IdentityKey {
        crate::identity::Identity::generate(&mut OsRng).public()
    }

    fn bob() -> IdentityKey {
        crate::identity::Identity::generate(&mut OsRng).public()
    }

    fn h(b: u8) -> Hash {
        let arr = [b; 32];
        Hash::from(arr)
    }

    #[test]
    fn apply_event_inserts_first_event() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        let changed = apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        assert!(changed, "first event should mark target as changed");
        let snap = derive_snapshot(&state, &target);
        let alice_set = snap.get("👍").unwrap();
        assert!(alice_set.contains(&alice));
    }

    #[test]
    fn apply_event_lww_later_timestamp_wins() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        // Add at t=100
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        // Remove at t=200 — later, wins.
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Remove,
                sent_at_ms: 200,
                value_hash: h(20),
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(
            snap.get("👍").map(|s| s.is_empty()).unwrap_or(true),
            "Remove at later timestamp should evict author"
        );
    }

    #[test]
    fn apply_event_lww_earlier_timestamp_loses() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        // Add at t=200
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 200,
                value_hash: h(10),
            },
        );
        // Stale Remove at t=100 — earlier, loses.
        let changed = apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Remove,
                sent_at_ms: 100,
                value_hash: h(20),
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(snap.get("👍").unwrap().contains(&alice), "stale Remove must not evict");
        let _ = changed;
    }

    #[test]
    fn apply_event_value_hash_breaks_timestamp_tie() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        let lower = h(0x05);
        let higher = h(0x50);
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: lower,
            },
        );
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Remove,
                sent_at_ms: 100,
                value_hash: higher,
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(
            snap.get("👍").map(|s| s.is_empty()).unwrap_or(true),
            "higher value_hash at same timestamp should win (Remove evicts)"
        );
    }

    #[test]
    fn apply_event_independent_authors_coexist() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        let bob = bob();
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        apply_event(
            &mut state,
            ReactionEvent {
                author: bob.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(11),
            },
        );
        let snap = derive_snapshot(&state, &target);
        let set = snap.get("👍").unwrap();
        assert!(set.contains(&alice));
        assert!(set.contains(&bob));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn apply_event_independent_emoji_coexist() {
        let mut state = ReactionState::new();
        let target = h(1);
        let alice = alice();
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "👍".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 100,
                value_hash: h(10),
            },
        );
        apply_event(
            &mut state,
            ReactionEvent {
                author: alice.clone(),
                target,
                emoji: "🎉".to_owned(),
                action: ReactionAction::Add,
                sent_at_ms: 101,
                value_hash: h(11),
            },
        );
        let snap = derive_snapshot(&state, &target);
        assert!(snap.get("👍").unwrap().contains(&alice));
        assert!(snap.get("🎉").unwrap().contains(&alice));
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;
    use rand_core::OsRng;

    fn alice() -> IdentityKey {
        crate::identity::Identity::generate(&mut OsRng).public()
    }

    #[test]
    fn signature_equal_for_equivalent_snapshots() {
        let mut a = ReactionSnapshot::new();
        let mut b = ReactionSnapshot::new();
        let alice = alice();
        a.entry("👍".to_owned()).or_default().insert(alice.clone());
        b.entry("👍".to_owned()).or_default().insert(alice.clone());
        assert_eq!(reactions_signature(&a), reactions_signature(&b));
    }

    #[test]
    fn signature_changes_when_emoji_added() {
        let mut a = ReactionSnapshot::new();
        let alice = alice();
        a.entry("👍".to_owned()).or_default().insert(alice.clone());
        let s1 = reactions_signature(&a);
        a.entry("🎉".to_owned()).or_default().insert(alice.clone());
        let s2 = reactions_signature(&a);
        assert_ne!(s1, s2);
    }

    #[test]
    fn signature_changes_when_author_added() {
        let mut a = ReactionSnapshot::new();
        let key1 = alice();
        let key2 = alice(); // distinct identity
        a.entry("👍".to_owned()).or_default().insert(key1);
        let s1 = reactions_signature(&a);
        a.entry("👍".to_owned()).or_default().insert(key2);
        let s2 = reactions_signature(&a);
        assert_ne!(s1, s2);
    }

    #[test]
    fn signature_stable_under_iteration_order() {
        let key1 = alice();
        let key2 = alice();
        let key3 = alice();
        let mut snap = ReactionSnapshot::new();
        for author in [key1.clone(), key2.clone(), key3.clone()] {
            snap.entry("👍".to_owned()).or_default().insert(author);
        }
        let s1 = reactions_signature(&snap);
        let mut snap2 = ReactionSnapshot::new();
        for author in [key3, key1, key2] {
            snap2.entry("👍".to_owned()).or_default().insert(author);
        }
        let s2 = reactions_signature(&snap2);
        assert_eq!(s1, s2);
    }
}
