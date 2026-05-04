# 3-peer WebRTC mesh sequence enumeration

Audience: anyone debugging the browser WebRTC signaling dispatcher in
`crates/sunset-sync-webrtc-browser/src/wasm.rs`. The current dispatcher
has a known race that wedges the 3rd peer in a 3-way voice call; this
note enumerates every sequence we expect to handle, identifies which
the current code mishandles, and verifies the proposed parallel
per-peer accept architecture against the same enumeration.

## Cast of characters

Three peers, lex-ordered by Ed25519 public key: **A < B < C**.

Glare-avoidance rule (`crates/sunset-voice/src/runtime/auto_connect.rs`):
> only the peer with the lexicographically smaller public key dials.

So the dialing matrix is:

| Peer | Dials | Accepts from |
|------|-------|--------------|
| A    | B, C  | (none)       |
| B    | C     | A            |
| C    | (none)| A, B         |

Three connections form: A↔B (A dials), A↔C (A dials), B↔C (B dials).

## Layered architecture refresher

For each handshake X→Y (X dials):

1. X's auto_connect FSM transitions Unknown→Dialing on first observed
   voice-presence entry from Y.
2. X calls `WebDialer::ensure_direct(Y)` →
   `OpenRoom::connect_direct(Y)` →
   `PeerSupervisor::add(Direct(webrtc://Y))`.
3. The supervisor spawns a dial task: `engine.add_peer` →
   `WebRtcRawTransport::connect(Y)`.
4. `connect()` registers `per_peer[Y]`, builds RTCPeerConnection with
   two datachannels (sunset-sync, sunset-sync-unrel), creates+sends
   an Offer through the signaler, and loops on the per_peer queue +
   ICE forwarder until both datachannels open.
5. Y's `WebRtcRawTransport`:
   - The shared dispatcher (`ensure_dispatcher`) drains
     `signaler.recv()`. Offers are pushed onto `offers_tx`; Answer/ICE
     are routed to `per_peer[from]` if registered, otherwise dropped.
   - The single-task accept worker (`ensure_accept_worker`) drains
     `offers_rx` one at a time. For each offer it inserts
     `per_peer[from]`, runs the full handshake, and on completion
     pushes the Connection onto `completed_tx`.
6. The engine's `accept` loop reads from `completed_rx` and runs
   the Hello exchange to complete the peer.

**Key invariant**: only when `per_peer[X]` is registered will inbound
non-Offer signals (Answer, ICE) for X reach the handshake. If they
arrive before `per_peer[X]` exists, **the dispatcher drops them silently**
(`wasm.rs:127–130`).

## Enumeration axes

Five independent axes drive the meaningful sequence space:

1. **Voice-presence observation order** at each peer's local subscriber.
2. **WebRTC dial timing** (which dial each peer fires first).
3. **Signaling message arrival order** at each peer's dispatcher.
4. **Accept worker scheduling** (ordering of inbound offers per peer).
5. **Late joiner vs. concurrent joiners**.

Plus failure / retry sequencing.

## Enumerated cases

### Case 1: idealised serial bootstrap

A starts, B starts a few ms later, C starts last (≥ 200 ms gap each so
voice-presence has time to propagate before the next peer joins).

Sub-cases:

- **1a**: A and B finish their handshake before C joins. C joins → C's
  voice-presence observer sees both A and B; A and B dial C;
  C's accept worker processes A's offer, then B's offer (or vice
  versa). Each pair establishes cleanly.
- **1b**: As 1a, except A and B's voice-presence subscribers see C
  before C is ready to receive. (Impossible in steady state — C
  publishes voice-presence in `voice_presence_publisher` only after
  the runtime is up, and the runtime is up before the dispatcher /
  accept worker, so by the time A/B see C's presence, C's dispatcher
  is already running.)

**Today's behavior**: 1a works. 1b is structurally impossible.
**Proposed behavior**: same.

### Case 2: A dials B then C immediately

A's voice-presence subscriber sees B and C effectively simultaneously
(within one bus-event tick). Auto-connect FSM transitions B then C —
or C then B — to Dialing in some order. Each transition fires
`ensure_direct` which calls `connect_direct`, registering an intent
with the supervisor and spawning a dial task per intent.

The two dial tasks run concurrently in A. Order of operations on the
wire:

1. A.connect(B) registers per_peer[B], sends Offer→B, spawns
   ICE forwarder.
2. A.connect(C) registers per_peer[C], sends Offer→C, spawns
   ICE forwarder.

These are interleaved at the A → relay write granularity. Both Offers
go through the `signaler`'s ordered per-(from,to) seq counter, so
ordering with respect to one peer is preserved, but B and C's offers
are independent.

A's per_peer[B] and per_peer[C] are registered before either Offer
sends → answers + ICE for either peer reach the right queue.

**Today's behavior**: works. Both connect-side handshakes complete in
parallel because each is a `spawn_local`'d future under the supervisor.
**Proposed behavior**: same.

### Case 3: B's accept-side races with B's dial of C

This is the crux. B's voice-presence subscriber may see C and A in any
order. Suppose:

3.1. B sees C first. B (lex-smaller than C) calls
     `connect_direct(C)`. B.connect(C) → per_peer[C] registered,
     Offer→C sent.
3.2. B sees A. A is lex-smaller than B, so B does NOT dial. B waits
     to receive A's Offer.
3.3. Meanwhile A has dialed B. A's Offer→B was sent before B even
     subscribed (or between subscribe and the FSM transition).
3.4. A's Offer arrives at B's dispatcher → pushed to offers_tx.
3.5. B's accept worker (sequential) wakes up, registers per_peer[A]
     (already empty), runs handshake A. B's accept worker is busy
     until A↔B fully completes.

Now: while B's accept worker is busy with A, a fresh Offer arrives at
B from… nobody, because A is the only peer that ever offers to B
(C doesn't dial B because C > B).

**No issue here**, since the only offerer to B is A. B's single accept
worker handles A's offer, that's the only one it gets. ✅ today.

### Case 4: C receives Offers from BOTH A and B

C is the only peer that can receive multiple inbound offers.

Sequence:
1. A.connect(C) sends Offer_A→C.
2. B.connect(C) sends Offer_B→C.
3. C's dispatcher pushes Offer_A and Offer_B onto offers_tx in arrival
   order. Suppose Offer_A arrives first.
4. C's accept worker pops Offer_A, registers per_peer[A], runs the
   full handshake A↔C.
5. While the accept worker is busy with A↔C, Offer_B sits in
   offers_rx waiting.
6. **Critical**: B starts sending ICE for the B↔C handshake once it
   has its own RTCPeerConnection. Those ICE messages arrive at C's
   dispatcher. C's dispatcher checks `per_peer[B]` — **NOT YET
   REGISTERED** (B's offer is still in the offers_rx queue). The
   dispatcher silently drops them (line 127–130 of wasm.rs).
7. Eventually A↔C completes; the accept worker pops Offer_B, registers
   per_peer[B], starts the handshake. Now the ICE forwarder on C's
   side starts producing local ICE candidates and sending them to B.
   But:
   - The early ICE that B sent (step 6) is gone.
   - C must wait for B to send a fresh round of ICE. Browsers DO
     emit additional ICE candidates over time as STUN/TURN probing
     proceeds, so this isn't necessarily fatal — but it can stretch
     handshake setup well past the test budget.
   - Worst case: if B has already emitted all its useful candidates
     while C was busy and no further candidates are forthcoming
     before the test/UX times out, the handshake never completes.

This is the **wedge** the current code mishandles.

**Today's behavior**: best-case the handshake limps in via late
trickle ICE. Worst-case it never completes. The 3-way voice test fails
intermittently on this race.
**Proposed behavior**: per-peer accept tasks run in parallel; ICE
candidates land in a per-peer buffer the moment they arrive; the
spawned per-peer task drains the buffer when it starts. No drops.

### Case 5: All three peers join concurrently

`voice_start` fires on A, B, C within a few milliseconds. All three
publish voice-presence near-simultaneously. The relay broadcasts each
to the other two.

Permutations of "first observation" per peer: 3! = 6 (which entry
arrives first at A's subscriber, etc.). But only two outcomes matter:

5.1. A sees B before C, dials B; later sees C, dials C.
5.2. A sees C before B, dials C; later sees B, dials B.

Same for B (only dials C). C never dials.

In all permutations, C will receive Offer_A and Offer_B — same
problem as Case 4. The arrival order at C may now be either
{A, B} or {B, A}; whichever comes second waits in the queue and
loses early ICE.

**Today's behavior**: same wedge as Case 4.
**Proposed behavior**: same fix as Case 4.

### Case 6: late joiner C

A and B are already in voice for ~5 s; C joins. C's voice-presence
publish reaches A and B via the relay; A and B dial C. Since A and B
are independent, both dials race to C. Same Case 4 problem.

**Today's behavior**: same wedge.
**Proposed behavior**: fixed.

### Case 7: B's accept of A's offer is fast; B's connect to C overlaps

Pure variant of Case 3 with overlap. No new failure mode.

### Case 8: ICE candidate race at the dialing side

When A dials B, A's `connect()` starts spawning the ICE forwarder
**after** sending the Offer. B's accept-worker handshake may produce
a remote ICE before A's per_peer[B] is registered. But A registers
`per_peer[B]` before sending the Offer (line 156), so this can't
happen on the connect side.

Actually wait — A's connect() registers per_peer[B] at line 156, then
sends the Offer at line 211. So inbound for A from B (Answer + ICE)
always lands in A's per_peer[B] queue. ✅

What about the Offer→Answer race in the other direction? The accept
worker on B inserts per_peer[A] **after** receiving the Offer (line
364). Between A sending the Offer and the accept worker registering
per_peer[A], any ICE A sends to B will be dropped. But A doesn't
emit any ICE until after `setLocalDescription`, and ICE trickling
typically begins ~ms later. There's still a window: Offer in flight
to B → relay forwarded → B's dispatcher pushes to offers_tx → accept
worker pops it → setRemoteDescription → registers per_peer[A]. The
register happens AT ENTRY to `run_accept_one` (line 364) **before**
setRemoteDescription (line 429). So in the current code, B's accept
worker registers per_peer[A] before doing any await on the WebRTC
APIs. Good — so as long as offers_rx isn't backlogged, ICE has
somewhere to land.

But if multiple offers are pending (Case 4 / 5), only the head of the
queue gets per_peer registered; subsequent offers' peers have ICE
dropped until the worker finishes the previous handshake.

### Case 9: Reconnect after WebRTC failure

Suppose A↔B's handshake fails halfway: e.g. ICE never gathers any
useful candidate (NAT type combo), or a datachannel error. The
`run_accept_one` Result is Err; the accept worker pushes it onto
`completed_tx`; the engine's `accept` loop returns Err; the `run_peer`
task on the engine side never starts. The supervisor sees no
`PeerAdded` event for that intent (because the connection never
completed). On the dialing side, `connect()` errors → `add_peer`
errors → supervisor enters Backoff and retries with a fresh Direct
intent.

When the retry fires:
- A's connect() registers a fresh per_peer[B] — but the OLD
  per_peer[B] entry was unregistered at end of the previous attempt
  (line 276 / 500), so it's gone. ✅
- B's accept worker is still alive (it loops forever) and ready for
  the next offer.
- But what about stale ICE that B's previous failed handshake emitted?
  B's previous accept-side run_accept_one terminated with an error,
  so its ICE forwarder task (spawn_local'd at line 452) — wait, that
  task continues running until the underlying ice_rx channel is
  closed. The channel is closed when `ice_tx` is dropped, which
  happens when the RtcPeerConnection's onicecandidate closure is
  dropped, which happens when `_on_ice` is dropped — but `_on_ice`
  is held in the WebRtcRawConnection. If we never built the
  Connection (failed mid-handshake), `_on_ice` and the rest are dropped
  at the end of `run_accept_one`'s scope. So the ice_tx is dropped,
  the forwarder exits, no stale ICE leaks into the next attempt.
  ✅

So reconnect today works correctly **for the connection-side state**.
The wedge (Case 4) is independent.

### Case 10: Glare violation (lex tiebreak fails)

The auto_connect code skips dialing if `self_pk >= peer_pk`
(line 89). So glare is impossible at the auto_connect layer.

But: what if a manual `connect_direct` is called outside auto_connect?
Hosts may call this directly (e.g. user clicks "connect to peer" in
UI). Then both sides may legitimately dial. The current code's only
defense is `WebRtcSignalKind::Offer(_) | Answer(_)` arm at line 268
which "ignores duplicate Offer", and the same for the accept side
implicitly via the per_peer registry. We'd end up with two
handshakes per pair colliding — the same scenario that motivated
the lex tiebreak.

**Today's behavior**: bad if both sides manually dial (out of scope
for the auto_connect pathway). Documented as a glare-avoidance contract
on the auto_connect side.
**Proposed behavior**: same — the per-peer accept architecture doesn't
fix the underlying glare issue. Recommendation: keep glare avoidance
as an upstream contract; if needed, fall back to Perfect Negotiation
later. Out of scope for this fix.

### Case 11: Late ICE after datachannel open

Browsers can emit ICE candidates after the datachannel is open
(continuing connectivity checks for path optimisation). Today,
`run_accept_one`'s loop exits as soon as both datachannels open
(line 495). After that, `peer_in_rx` is dropped (out of scope) and
per_peer[A] is removed (line 500). Subsequent ICE for that peer is
dropped silently by the dispatcher. Equivalently for the connect side
(line 276).

Is this a problem? Once a datachannel is open, dropping additional
ICE just prevents ICE restart / connectivity-check upgrades. It does
NOT tear down the connection. For the voice use case (5-minute calls,
typically), this is acceptable.

**Today's behavior**: late ICE silently dropped, connection stays up.
**Proposed behavior**: same. We accept the design choice — the per-peer
buffer is torn down when the handshake completes.

### Case 12: ICE arrives for a peer we never offered to / never received an offer from

Stray ICE for unknown peer X arrives at the dispatcher. Today: silently
dropped (per_peer.get returns None).

What could cause this? A peer who initiated a handshake, received a
crash / page reload before our Offer/Answer was processed, then
re-initiated. The previous connection's ICE is still in flight. Or
malicious / buggy peer. Or routing weirdness during reconnect (Case 9).

**Today's behavior**: dropped. ✅ no leak.
**Proposed behavior**: needs a buffer policy. See "Architecture
decision points" below.

## Summary of failure modes

| Case | Today | Proposed (parallel per-peer accept) |
|------|-------|-------------------------------------|
| 1 (serial) | ✅ | ✅ |
| 2 (A dials B+C) | ✅ | ✅ |
| 3 (B accepts A while dialing C) | ✅ | ✅ |
| **4 (C accepts A then B)** | **❌ wedge** | **✅ fixed** |
| **5 (concurrent join)** | **❌ wedge** | **✅ fixed** |
| **6 (late joiner C)** | **❌ wedge** | **✅ fixed** |
| 7 (overlap variant of 3) | ✅ | ✅ |
| 8 (dial-side ICE race) | ✅ | ✅ |
| 9 (reconnect after failure) | ✅ | ✅ (with care: see decision below) |
| 10 (glare) | out of scope | out of scope |
| 11 (late ICE after open) | drop, ok | drop, ok |
| 12 (stray ICE for unknown peer) | drop | **buffer with TTL or drop?** |

## Proposed architecture: parallel per-peer accept tasks

### Shape

Replace the single sequential accept worker with a per-peer dispatch
that spawns a fresh task on first inbound Offer:

```
Inner {
    dispatcher_started: bool,
    per_peer: HashMap<PeerId, mpsc::UnboundedSender<WebRtcSignalKind>>,
    completed_tx: mpsc::UnboundedSender<Result<WebRtcRawConnection>>,
    // NEW:
    early_ice: HashMap<PeerId, EarlyIceBuffer>,
}

struct EarlyIceBuffer {
    candidates: Vec<String>,
    inserted_at: web_time::Instant,
}
```

Dispatcher routing:
- **Offer(sdp)** from peer X:
  - If `per_peer[X]` exists → glare / duplicate (active connect or
    accept). Either ignore the Offer (current behaviour), or
    forward to the per_peer queue and let the connect-side decide
    (current code ignores it).
  - Otherwise: spawn a per-peer accept task for X. The task creates
    its own `(tx, rx)`, inserts `tx` into per_peer[X] **before any
    await**, drains `early_ice[X]` if present, and runs the
    handshake.
- **Answer(sdp)** or **IceCandidate(json)** from peer X:
  - If `per_peer[X]` exists → forward.
  - Otherwise: append to `early_ice[X]` (buffer for an offer/connect
    we expect to start soon).

Per-peer task lifecycle:
- Spawn on first inbound Offer.
- Lives until handshake completes (success → push to
  `completed_tx`) or fails (push Err to `completed_tx`).
- On completion (success OR failure), removes its `per_peer[X]`
  entry. Also drains/removes any `early_ice[X]` entry.

Connect-side (unchanged in spirit, but also writes to `early_ice` if
needed):
- `connect()` registers per_peer[X] **before sending Offer**
  (already does this — line 156). ✅
- After completion / failure, removes per_peer[X] (already does
  this — line 276). ✅
- early_ice[X] is irrelevant on the connect side because per_peer is
  registered first.

### Architecture decision points

#### Buffer TTL for early_ice

**Decision**: prune entries older than 30 seconds when the dispatcher
processes an event. Rationale:
- Realistic ICE trickling completes in 1–5 s. 30 s is generous.
- Cleanup happens piggybacked on dispatcher activity (no separate
  timer task).
- If no further dispatcher activity occurs, the buffer leaks until
  the next event or the transport drops. Bounded by the number of
  unique remote peers that have ever sent us stray signals — small
  in practice, finite always.

Alternative: per-peer accept task tears down its `early_ice[X]`
entry on entry (drains it then removes). Stray ICE for peers we
never accept from then leaks indefinitely. Worse.

Alternative: do not buffer early ICE at all; rely on the per-peer
accept task being spawned immediately on the Offer arrival, before
any ICE could be in flight. **This doesn't work** — the
RelaySignaler delivers signals one-at-a-time per send; per-peer ICE
trickling at the OFFERER side begins after `setLocalDescription`,
which is BEFORE the offer is sent in the first place. So ICE can
genuinely arrive before the Offer if the relay reorders or if the
ICE forwarder's first send beats the Offer's send (both happen on
parallel `spawn_local`'d tasks).

Wait — let me check that. In `connect()`:
- Line 195: createOffer
- Line 201: setLocalDescription (this triggers ICE gathering →
  closures fire → ICE pushed to ice_tx)
- Line 206: send Offer
- Line 216: spawn_ice_forwarder

So the ICE channel (ice_tx → ice_rx → forwarder → wire) is set up
**after** the offer is sent. ICE candidates queued in ice_tx during
the gap between `setLocalDescription` and the spawn of the forwarder
sit in the channel buffer; the forwarder drains them in order. By
the time the forwarder sends the first ICE on the wire, the Offer
is already on the wire (one signaler.send earlier, in the same
task, before the spawn). So at the receiver side, the Offer arrives
before any ICE for that handshake.

**But** there's still a race at the other peer's dispatcher: if
multiple peers' offers/ICE are interleaved at the dispatcher, the
single-task accept worker can pop one offer, get blocked on its
handshake, and miss a SECOND peer's ICE that arrives before that
peer's offer is processed. So the early_ice buffer is NEEDED for
the case where Offer_X arrives at the dispatcher BEFORE the per-peer
task for X is spawned (which today happens because of accept-worker
serialization). With parallel per-peer tasks, the spawn happens at
dispatcher level the moment the Offer is seen — but tasks may not
get scheduled before subsequent ICE arrives, so we still need the
buffer for safety.

**Confirmed: buffer is needed.** TTL is 30 s, pruned piggyback.

#### Retry interaction

When per-peer task X fails, it removes per_peer[X] and early_ice[X].
The supervisor's PeerSupervisor sees the failure (engine sees
`add_peer` Err → never PeerAdded → no PeerRemoved either — but the
connect-side path does propagate Err up to the supervisor which goes
into Backoff). On backoff timer expiry, supervisor calls
`add_peer` again → fresh `connect()` → fresh per_peer[X]. The
retry's ICE is independent of the prior attempt's ICE.

For the accept side: on failure, we removed per_peer[X] and
early_ice[X]. A fresh Offer from the same peer X (because their
supervisor also retries) will trigger a fresh accept-side per-peer
task. ICE that arrives between the failure and the new offer goes
into early_ice[X]; the new task drains it. Stale ICE from the FAILED
attempt is harmless to the new RtcPeerConnection because ICE
candidates are addressed to a (ufrag, pwd) pair specific to that
RTCPeerConnection's session; stale ICE for a stale session will be
rejected by `addIceCandidate`.

**But** the stale ICE will succeed-or-fail-via-error in the new
peer's `addIceCandidate` call — the latter logs a warning and
returns Err from our `add_remote_ice` helper, which would propagate
out of the `peer_in_rx.next()` arm and tear down the new handshake.
**This is bad.** We need to either:

- **Option A**: Tolerate `addIceCandidate` errors (log + continue
  rather than propagating).
- **Option B**: Tear down the per_peer entry and early_ice on failure
  immediately, so stale ICE that arrives during the gap is dropped
  by the dispatcher (no per_peer, no per-peer task to spawn it
  into).

Option B is what we'll do, BUT there's a small gap: the dispatcher
might enqueue stale ICE into early_ice[X] between the failure
cleanup and the new Offer. To handle this:

- **Option B+**: on Offer arrival for X, the dispatcher creates the
  per-peer task immediately AND clears any pre-existing early_ice[X]
  BEFORE the new task drains it. So the new task starts with no
  stale ICE. (This means we lose any LEGITIMATE early ICE — but
  the connect-side ICE forwarder's first send is always after the
  offer reaches the wire, so legitimate early ICE arriving before
  the offer at the same peer is rare. The peer will retransmit/keep
  trickling.)
- **Option B++**: harden `add_remote_ice` to log+continue on
  errors. Belt-and-suspenders.

**Decision**: do **Option A** (tolerate `addIceCandidate` errors —
warn and continue) AND keep the early_ice buffer fresh-per-Offer
(clear early_ice[X] when starting a new accept task for X). Both
together make stale ICE harmless.

#### Connect/accept race

`auto_connect.rs` enforces lex tiebreak at the auto_connect layer.
The dispatcher could still in principle see both an outbound Offer
(via connect) AND an inbound Offer (via dispatcher) for the same
peer. The connect-side registers per_peer[X] first. When the inbound
Offer arrives, the dispatcher would route it via the existing
per_peer[X] queue rather than spawn a new accept task. Today the
connect-side glare-arm at line 268 ignores the duplicate Offer —
preserve that.

**Decision**: dispatcher checks `per_peer[X]` on Offer arrival; if
present, route the Offer to the existing queue (current behavior:
the connect-side ignores it). If absent, spawn a per-peer accept
task. Symmetric: per_peer entry presence is the lock.

#### Late ICE after handshake completes

Same as today: per_peer[X] removed on completion; subsequent ICE
silently dropped. Acceptable for our use case.

### Verification against enumeration

| Case | Proposed handles correctly? | Notes |
|------|-----------------------------|-------|
| 1, 2, 3, 7, 8 | yes | Same as today; no regression |
| 4, 5, 6 (the wedge) | **yes** | Per-peer task spawned immediately; early ICE buffered until task drains |
| 9 (reconnect) | yes | per_peer cleanup on failure + tolerate stale ICE addIceCandidate errors |
| 10 (glare) | no, but out of scope | Lex tiebreak still handles it at auto_connect layer |
| 11 (late ICE after open) | yes | dropped silently as today |
| 12 (stray ICE for never-offered peer) | yes, with TTL | early_ice[X] grows until 30 s prune kicks in |

## Implementation outline

`crates/sunset-sync-webrtc-browser/src/wasm.rs`:

1. **Inner**: drop `accept_worker_started`, `offers_tx`, `offers_rx`.
   Keep `dispatcher_started`, `per_peer`, `completed_tx`. Add
   `early_ice: HashMap<PeerId, EarlyIceBuffer>`.

2. **`ensure_dispatcher`**: rewrite the dispatch loop:
   - Lazily prune `early_ice` entries older than 30 s on each event.
   - On `Offer(sdp)`:
     - If `per_peer[from]` exists: push the Offer onto that queue
       (or ignore — depends on whether glare-detection helps; we'll
       push so the connect-side can decide).
     - Otherwise:
       - Drain and clear `early_ice[from]`, hand the candidates to
         the new task.
       - `spawn_local(run_accept_one_task(...))` with all the
         dependencies cloned in.
       - Insert `per_peer[from] = tx`.
   - On `Answer(sdp)` / `IceCandidate(_)`:
     - If `per_peer[from]` exists: send via the channel.
     - Otherwise: append to `early_ice[from]`.

3. **`run_accept_one`**: refactor to take an initial set of
   pre-buffered ICE candidates as a parameter. Process them right
   after `setRemoteDescription` (before / after answer? — has to be
   after `setRemoteDescription` so `addIceCandidate` doesn't error
   "remote description was null"; actually wait — these are LOCAL
   side's not-yet-applied candidates; we apply them after
   setRemoteDescription on the offer). Make `add_remote_ice` calls
   inside the buffered loop tolerant: log + continue rather than
   bubbling up the Err.

4. **`accept`**: unchanged — still drains `completed_rx`. The thing
   that changed is who feeds it: now per-peer tasks rather than the
   single accept worker.

5. **Drop `ensure_accept_worker`** and the offers_tx/offers_rx plumbing
   — replaced by direct spawn at the dispatcher.

6. **add_remote_ice tolerance**: harden the function to return Ok on
   parse error / addIceCandidate error (warn-and-continue), per
   "Option A" above. Or alternatively, only tolerate inside the
   buffered-ICE drain loop.

   I'll go with: **`add_remote_ice` keeps its current semantics
   (returns Err on browser failure)**; the call sites inside
   `run_accept_one` for ICE coming through `peer_in_rx` log+continue
   instead of `?`, AND the buffered-ICE drain at task start does the
   same. The connect-side `add_remote_ice` calls in `connect()`
   should similarly log+continue. This isolates the fault tolerance
   to the WebRTC layer's own decisions and avoids one bad candidate
   killing the handshake.

## Open questions for the controller

1. Should the dispatcher route a duplicate Offer (from a peer we're
   already accept-handshaking with) to the existing per_peer queue
   (so the existing handshake can decide), or drop it? **My
   recommendation**: route it through. The connect-side already has
   a glare-arm that ignores duplicate Offers (`wasm.rs:268`); the
   accept-side currently has no equivalent. We can add a noop arm in
   the accept-side loop.

2. Tolerating `addIceCandidate` errors instead of bubbling up — is
   that OK? **My recommendation**: yes. ICE candidates are
   speculative; one bad candidate is not a connection failure.

3. Buffer TTL of 30 s — sane? **My recommendation**: yes. Could be
   parameterised via the constructor if anyone needs to tune it
   later, but a constant is fine for now.

If you say go, I implement.
