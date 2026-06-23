import gleeunit/should
import sunset_web/voice_volume_cache as cache

// --- empty / basic round-trip ---

pub fn empty_cache_get_is_error_test() {
  cache.new(20)
  |> cache.get("aa")
  |> should.equal(Error(Nil))
}

pub fn set_then_get_returns_value_test() {
  cache.new(20)
  |> cache.set("aa", 300)
  |> cache.get("aa")
  |> should.equal(Ok(300))
}

pub fn set_existing_updates_value_in_place_test() {
  let c =
    cache.new(20)
    |> cache.set("aa", 300)
    |> cache.set("bb", 50)
    |> cache.set("aa", 150)
  // value updated...
  cache.get(c, "aa") |> should.equal(Ok(150))
  // ...and order is preserved (aa stays ahead of bb; updating does
  // not move it to the newest position — strict FIFO, not LRU).
  cache.to_list(c) |> should.equal([#("aa", 150), #("bb", 50)])
}

// --- FIFO eviction ---

pub fn overflow_evicts_oldest_test() {
  // Capacity 3. Insert four distinct peers — the first-inserted ("a")
  // is the oldest and must be dropped, leaving [b, c, d] in order.
  let c =
    cache.new(3)
    |> cache.set("a", 10)
    |> cache.set("b", 20)
    |> cache.set("c", 30)
    |> cache.set("d", 40)
  cache.get(c, "a") |> should.equal(Error(Nil))
  cache.to_list(c)
  |> should.equal([#("b", 20), #("c", 30), #("d", 40)])
}

pub fn updating_existing_does_not_count_against_capacity_test() {
  // Full cache of 3. Re-setting an existing key updates in place and
  // must NOT evict anyone (length stays at capacity).
  let c =
    cache.new(3)
    |> cache.set("a", 10)
    |> cache.set("b", 20)
    |> cache.set("c", 30)
    |> cache.set("b", 99)
  cache.get(c, "a") |> should.equal(Ok(10))
  cache.to_list(c)
  |> should.equal([#("a", 10), #("b", 99), #("c", 30)])
}

pub fn touch_does_not_refresh_eviction_order_test() {
  // Strict FIFO: re-setting "a" (the oldest) does not save it from
  // being the next evicted when a genuinely new key arrives.
  let c =
    cache.new(3)
    |> cache.set("a", 10)
    |> cache.set("b", 20)
    |> cache.set("c", 30)
    |> cache.set("a", 11)
    |> cache.set("d", 40)
  // "a" was touched most recently but is still the oldest by insertion,
  // so it — not "b" — is evicted.
  cache.get(c, "a") |> should.equal(Error(Nil))
  cache.to_list(c)
  |> should.equal([#("b", 20), #("c", 30), #("d", 40)])
}

// --- serialization round-trip (localStorage <-> cache) ---

pub fn from_list_preserves_order_test() {
  cache.from_list(20, [#("a", 10), #("b", 20)])
  |> cache.to_list
  |> should.equal([#("a", 10), #("b", 20)])
}

pub fn from_list_keeps_only_most_recent_capacity_test() {
  // A persisted blob longer than capacity (e.g. capacity was larger
  // when written) is trimmed to the most recent entries on load,
  // dropping the oldest from the front.
  cache.from_list(2, [#("a", 10), #("b", 20), #("c", 30)])
  |> cache.to_list
  |> should.equal([#("b", 20), #("c", 30)])
}

pub fn default_capacity_is_twenty_test() {
  cache.default_capacity |> should.equal(20)
}
