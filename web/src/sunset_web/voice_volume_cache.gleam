//// Bounded FIFO cache of the per-peer playback volumes the user has
//// chosen, persisted across reloads. Keyed by a peer's verifying-key
//// hex; the value is the user-facing volume percent (the same integer
//// the popover slider shows). See `voice_volume` for the percent↔gain
//// curve.
////
//// Eviction is first-in-first-out, bounded by `capacity`: when a new
//// peer's volume is recorded and the cache is full, the oldest-inserted
//// entry is dropped. Re-recording a peer already in the cache updates
//// its value in place and does NOT refresh its position — strict FIFO,
//// not LRU. So the peers you keep are the 20 you most recently met, not
//// the 20 you most recently adjusted.

import gleam/list

/// Default bound on distinct remembered peers. Sized so the cache holds
/// roughly a session's worth of voice peers without growing localStorage
/// without limit.
pub const default_capacity: Int = 20

pub opaque type VolumeCache {
  // `entries` is ordered oldest-first: the head is the next eviction
  // victim, the tail is the most recently inserted peer.
  VolumeCache(capacity: Int, entries: List(#(String, Int)))
}

/// An empty cache bounded to `capacity` distinct peers.
pub fn new(capacity: Int) -> VolumeCache {
  VolumeCache(capacity: capacity, entries: [])
}

/// Record `percent` as the chosen volume for `hex`. If `hex` is already
/// present its value is updated in place (insertion order preserved);
/// otherwise it becomes the newest entry, evicting the oldest if that
/// pushes the cache past `capacity`.
pub fn set(cache: VolumeCache, hex: String, percent: Int) -> VolumeCache {
  // `key_set` updates in place when present and appends at the tail when
  // absent — exactly the FIFO insertion order this cache wants.
  let updated = list.key_set(cache.entries, hex, percent)
  VolumeCache(..cache, entries: trim_to_capacity(updated, cache.capacity))
}

/// The chosen volume percent for `hex`, or `Error(Nil)` if this peer
/// isn't remembered.
pub fn get(cache: VolumeCache, hex: String) -> Result(Int, Nil) {
  list.key_find(cache.entries, hex)
}

/// The remembered peers as `(hex, percent)` pairs, oldest-first. This is
/// the order written to localStorage so a reload reconstructs the same
/// eviction queue.
pub fn to_list(cache: VolumeCache) -> List(#(String, Int)) {
  cache.entries
}

/// Rebuild a cache from a persisted, oldest-first list of pairs, bounded
/// to `capacity`. A blob longer than `capacity` (e.g. written when the
/// bound was larger) is trimmed to its most recent entries.
pub fn from_list(capacity: Int, pairs: List(#(String, Int))) -> VolumeCache {
  VolumeCache(capacity: capacity, entries: trim_to_capacity(pairs, capacity))
}

// Drop the oldest entries (from the front) until at most `capacity`
// remain. A no-op when already within bound.
fn trim_to_capacity(
  entries: List(#(String, Int)),
  capacity: Int,
) -> List(#(String, Int)) {
  let overflow = list.length(entries) - capacity
  case overflow > 0 {
    True -> list.drop(entries, overflow)
    False -> entries
  }
}
