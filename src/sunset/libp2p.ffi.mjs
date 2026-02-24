import { createLibp2p } from "libp2p";
import { identify } from "@libp2p/identify";
import { noise } from "@chainsafe/libp2p-noise";
import { yamux } from "@chainsafe/libp2p-yamux";
import { multiaddr } from "@multiformats/multiaddr";
import { webSockets } from "@libp2p/websockets";
import { webTransport } from "@libp2p/webtransport";
import { webRTC } from "@libp2p/webrtc";
import { circuitRelayTransport } from "@libp2p/circuit-relay-v2";
import {
  WebRTC,
  WebSockets,
  WebSocketsSecure,
  WebTransport,
  Circuit,
  WebRTCDirect,
} from "@multiformats/multiaddr-matcher";
import { toList } from "../gleam.mjs";

let _libp2p = null;

// -- Monkey-patch RTCPeerConnection to prevent libp2p from killing
//    connections on transient ICE disconnections --
//
// libp2p's WebRTC transport listens for connectionstatechange and
// immediately closes the connection when the state is "disconnected".
// ICE "disconnected" is a transient state that often recovers within
// seconds (e.g. during SDP renegotiation after addTrack).  We wrap
// addEventListener so that handlers for connectionstatechange get a
// delayed notification for "disconnected" — if the connection recovers
// within the grace period, the handler never sees "disconnected".
const ICE_DISCONNECT_GRACE_MS = 5000;
const _originalAddEventListener = RTCPeerConnection.prototype.addEventListener;
const _disconnectTimers = new WeakMap(); // PC -> Map<listener, timerId>

RTCPeerConnection.prototype.addEventListener = function (type, listener, options) {
  if (type === "connectionstatechange" || type === "iceconnectionstatechange") {
    const wrappedListener = (event) => {
      const state =
        type === "iceconnectionstatechange"
          ? this.iceConnectionState
          : this.connectionState;

      if (state === "disconnected") {
        // Delay the callback — give ICE time to recover.
        let timers = _disconnectTimers.get(this);
        if (!timers) {
          timers = new Map();
          _disconnectTimers.set(this, timers);
        }
        const timerId = setTimeout(() => {
          timers.delete(listener);
          // Only fire if still disconnected (or worse).
          const currentState =
            type === "iceconnectionstatechange"
              ? this.iceConnectionState
              : this.connectionState;
          if (
            currentState === "disconnected" ||
            currentState === "failed" ||
            currentState === "closed"
          ) {
            listener.call(this, event);
          }
        }, ICE_DISCONNECT_GRACE_MS);
        timers.set(listener, timerId);
        return;
      }

      // If the connection recovered (e.g. back to "connected"), cancel
      // any pending delayed "disconnected" callbacks.
      if (state === "connected" || state === "completed") {
        const timers = _disconnectTimers.get(this);
        if (timers) {
          for (const [fn, tid] of timers) {
            clearTimeout(tid);
          }
          timers.clear();
        }
      }

      listener.call(this, event);
    };
    // Store mapping so removeEventListener works (best-effort).
    if (!this._wrappedListeners) this._wrappedListeners = new Map();
    this._wrappedListeners.set(listener, wrappedListener);
    return _originalAddEventListener.call(this, type, wrappedListener, options);
  }
  return _originalAddEventListener.call(this, type, listener, options);
};

const _originalRemoveEventListener =
  RTCPeerConnection.prototype.removeEventListener;
RTCPeerConnection.prototype.removeEventListener = function (
  type,
  listener,
  options,
) {
  if (
    (type === "connectionstatechange" ||
      type === "iceconnectionstatechange") &&
    this._wrappedListeners
  ) {
    const wrapped = this._wrappedListeners.get(listener);
    if (wrapped) {
      this._wrappedListeners.delete(listener);
      return _originalRemoveEventListener.call(this, type, wrapped, options);
    }
  }
  return _originalRemoveEventListener.call(this, type, listener, options);
};

// setTimeout wrapper for Gleam FFI
export function set_timeout(callback, ms) {
  setTimeout(callback, ms);
}

// Create and start a libp2p node. Returns a Promise that resolves
// once the node is online. Calls `dispatch` with the peer ID string.
export function init_libp2p(dispatch) {
  createLibp2p({
    addresses: {
      listen: ["/p2p-circuit", "/webrtc"],
    },
    transports: [
      webSockets(),
      webTransport(),
      webRTC(),
      circuitRelayTransport(),
    ],
    connectionEncrypters: [noise()],
    streamMuxers: [yamux()],
    connectionGater: {
      denyDialMultiaddr: async () => false,
    },
    services: {
      identify: identify(),
    },
  })
    .then((libp2p) => {
      _libp2p = libp2p;
      globalThis.libp2p = libp2p;

      libp2p.addEventListener("peer:connect", (event) => {
        addTrackToNewPeer(event.detail);
      });
      libp2p.addEventListener("peer:disconnect", () => {});

      dispatch(libp2p.peerId.toString());
    })
    .catch((err) => {
      console.error("Failed to create libp2p node:", err);
    });
}

// Dial a multiaddr string. Calls on_ok() on success, on_error(msg) on failure.
export function dial_multiaddr(addr_str, on_ok, on_error) {
  if (!_libp2p) {
    on_error("libp2p not initialised");
    return;
  }
  try {
    const maddr = multiaddr(addr_str);
    _libp2p
      .dial(maddr)
      .then(() => on_ok())
      .catch((err) => on_error(err.toString()));
  } catch (err) {
    on_error(err.toString());
  }
}

// Get the list of this node's multiaddrs as a Gleam List of strings.
export function get_multiaddrs() {
  if (!_libp2p) return toList([]);
  return toList(_libp2p.getMultiaddrs().map((ma) => ma.toString()));
}

// Get the list of connected peer IDs as a Gleam List of strings.
export function get_connected_peers() {
  if (!_libp2p) return toList([]);
  return toList(_libp2p.getPeers().map((p) => p.toString()));
}

// Get connection count.
export function get_connection_count() {
  if (!_libp2p) return 0;
  return _libp2p.getConnections().length;
}

// Get connection details: returns a Gleam List of #(peer_id, transport, remote_addr).
export function get_connection_details() {
  if (!_libp2p) return toList([]);

  const conns = _libp2p.getConnections();
  const details = conns.map((conn) => {
    const peerId = conn.remotePeer.toString();
    const ma = conn.remoteAddr;
    let transport = "Other";

    if (WebRTC.exactMatch(ma)) transport = "WebRTC";
    else if (WebRTCDirect.exactMatch(ma)) transport = "WebRTC Direct";
    else if (WebSocketsSecure.exactMatch(ma)) transport = "WebSockets (secure)";
    else if (WebSockets.exactMatch(ma)) transport = "WebSockets";
    else if (WebTransport.exactMatch(ma)) transport = "WebTransport";
    else if (Circuit.exactMatch(ma)) transport = "Circuit Relay";

    return [peerId, transport, ma.toString()];
  });

  return toList(details.map((d) => toList(d)));
}

// -- Chat protocol --

const CHAT_PROTOCOL = "/sunset/chat/1.0.0";
let _onChatMessage = null;

// Register the chat protocol handler. Must be called after init_libp2p.
// on_message receives (sender_peer_id, message_text).
export function register_chat_handler(on_message) {
  _onChatMessage = on_message;
  if (!_libp2p) return;
  _libp2p.handle(CHAT_PROTOCOL, async (stream, connection) => {
    try {
      const chunks = [];
      for await (const chunk of stream) {
        chunks.push(chunk.subarray());
      }
      const bytes = new Uint8Array(
        chunks.reduce((acc, c) => acc + c.length, 0),
      );
      let offset = 0;
      for (const c of chunks) {
        bytes.set(c, offset);
        offset += c.length;
      }
      const text = new TextDecoder().decode(bytes);
      const sender = connection.remotePeer.toString();
      if (_onChatMessage) _onChatMessage(sender, text);
    } catch (err) {
      console.error("Chat receive error:", err);
    }
  }, { runOnLimitedConnection: true });
}

// Broadcast a message to all connected peers, excluding the relay.
// Calls on_ok() when all sends are attempted, on_error(msg) on failure.
export function broadcast_message(text, on_ok, on_error) {
  if (!_libp2p) {
    on_error("libp2p not initialised");
    return;
  }
  const relayPeerId = _getRelayPeerId();
  const relayStr = relayPeerId ? relayPeerId.toString() : null;
  const peers = _libp2p.getPeers().filter((p) => p.toString() !== relayStr);
  if (peers.length === 0) {
    on_error("No peers connected");
    return;
  }
  const encoded = new TextEncoder().encode(text);
  const sends = peers.map(async (peerId) => {
    try {
      const stream = await _libp2p.dialProtocol(peerId, CHAT_PROTOCOL, { runOnLimitedConnection: true });
      await stream.send(encoded);
      await stream.close();
    } catch (err) {
      console.warn(`Failed to send to ${peerId}:`, err);
    }
  });
  Promise.all(sends)
    .then(() => on_ok())
    .catch((err) => on_error(err.toString()));
}

// -- Audio (via SDP renegotiation on libp2p's RTCPeerConnection) --

const SIGNALING_PROTOCOL = "/sunset/signaling/1.0.0";

let _localStream = null;
const _remoteAudios = new Map(); // RTCPeerConnection -> HTMLAudioElement
let _senders = []; // { pc, sender, peerId } references for cleanup
const _pcToPeer = new Map(); // RTCPeerConnection -> PeerId object
const _attachedPCs = new Set(); // PCs we've already attached listeners to
const _negotiationBusy = new WeakSet(); // PCs with an in-flight negotiation
const POST_GLARE_COOLDOWN_MS = 2000; // Cooldown after glare resolution before re-offering
let _audioJoined = false; // Whether user has opted in to hear remote audio

// Get the RTCPeerConnection from a libp2p connection object.
// Uses internal property path: conn.maConn.peerConnection
// (TypeScript `private` compiles to plain JS properties.)
function getPeerConnection(conn) {
  return conn.maConn?.peerConnection ?? null;
}

// Get all WebRTC connections with their peer IDs and RTCPeerConnections.
// Returns array of { peerId, pc, conn }.
// When a peer has multiple WebRTC connections, returns only the first
// (oldest/most established) one to avoid duplicate addTrack/negotiation.
function getWebRTCPeers() {
  if (!_libp2p) return [];
  const seen = new Set(); // peer ID strings already included
  const results = [];
  for (const conn of _libp2p.getConnections()) {
    // Only WebRTC connections have an RTCPeerConnection
    const pc = getPeerConnection(conn);
    if (pc == null) continue;
    const peerId = conn.remotePeer; // PeerId object, not string
    const pidStr = peerId.toString();
    _pcToPeer.set(pc, peerId);
    if (seen.has(pidStr)) continue;
    seen.add(pidStr);
    results.push({ peerId, pc, conn });
  }
  return results;
}

// Find the RTCPeerConnection for a given PeerId by scanning all connections.
// Returns null if the peer has no WebRTC connection (e.g. circuit relay only).
// When a peer has multiple WebRTC connections, returns the first live one.
function findPCForPeer(peerId) {
  if (!_libp2p) return null;
  const peerIdStr = peerId.toString();
  for (const conn of _libp2p.getConnections()) {
    if (conn.remotePeer.toString() !== peerIdStr) continue;
    const pc = getPeerConnection(conn);
    if (pc) return pc;
  }
  return null;
}

// Find ALL RTCPeerConnections for a given PeerId.
// Returns an array of PCs (may be empty).
function findAllPCsForPeer(peerId) {
  if (!_libp2p) return [];
  const peerIdStr = peerId.toString();
  const pcs = [];
  for (const conn of _libp2p.getConnections()) {
    if (conn.remotePeer.toString() !== peerIdStr) continue;
    const pc = getPeerConnection(conn);
    if (pc) pcs.push(pc);
  }
  return pcs;
}

// Check whether a given RTCPeerConnection still backs a live libp2p connection.
function isPCStillLive(pc) {
  if (!_libp2p) return false;
  for (const conn of _libp2p.getConnections()) {
    if (getPeerConnection(conn) === pc) return true;
  }
  return false;
}

// After a glare resolution (rollback + accept remote offer + send answer),
// negotiationneeded fires immediately because the original addTrack still
// needs SDP negotiation.  We keep _negotiationBusy held during a cooldown
// period so the immediate re-fire is suppressed, then trigger a deferred
// renegotiation once the signaling round-trip has settled.
//
// The timer is stored in _postGlareTimers so it can be cancelled/reset if
// another offer arrives during the cooldown (which resets the window).
const _postGlareTimers = new WeakMap(); // PC -> timerId

function schedulePostGlareRenegotiation(pc) {
  // Cancel any existing timer for this PC (reset the cooldown).
  const existing = _postGlareTimers.get(pc);
  if (existing) clearTimeout(existing);

  // _negotiationBusy is already set by the caller — keep it held.
  const timerId = setTimeout(async () => {
    _postGlareTimers.delete(pc);
    _negotiationBusy.delete(pc);

    const peerId = _pcToPeer.get(pc);
    if (!peerId || !isPCStillLive(pc)) return;
    if (pc.signalingState !== "stable") return;

    // Only renegotiate if we have local senders on this PC that may need
    // a new offer (i.e. the addTrack whose negotiation was rolled back).
    const hasPendingTracks = _senders.some((s) => s.pc === pc);
    if (!hasPendingTracks) return;

    console.log(
      `[PostGlare] Cooldown expired, triggering deferred renegotiation for ${peerId.toString().slice(-8)}`,
    );
    _negotiationBusy.add(pc);
    try {
      const iceState = pc.iceConnectionState;
      const needsRestart =
        iceState === "disconnected" ||
        iceState === "failed" ||
        iceState === "closed";
      const offer = await pc.createOffer({ iceRestart: needsRestart });
      await pc.setLocalDescription(offer);
      await sendSignalingMessage(peerId, {
        type: "offer",
        sdp: pc.localDescription.sdp,
      });
      console.log(
        `[PostGlare] Sent deferred renegotiation offer to ${peerId.toString().slice(-8)}`,
      );
    } catch (err) {
      console.error("[PostGlare] Deferred renegotiation failed:", err);
    } finally {
      _negotiationBusy.delete(pc);
    }
  }, POST_GLARE_COOLDOWN_MS);
  _postGlareTimers.set(pc, timerId);
}

// When a new peer connects while we're already broadcasting audio, add our
// audio track to their RTCPeerConnection.  The WebRTC transport upgrade may
// not be complete when peer:connect fires, so we retry a few times.
function addTrackToNewPeer(remotePeerId) {
  if (!_localStream) return; // not broadcasting
  const track = _localStream.getAudioTracks()[0];
  if (!track) return;

  let attempts = 0;
  const maxAttempts = 10;
  const intervalMs = 500;

  const timer = setInterval(() => {
    attempts++;
    // Find the connection for this peer and grab its RTCPeerConnection.
    const conns = _libp2p.getConnections(remotePeerId);
    let pc = null;
    for (const conn of conns) {
      pc = getPeerConnection(conn);
      if (pc) {
        _pcToPeer.set(pc, remotePeerId);
        break;
      }
    }
    if (!pc) {
      if (attempts >= maxAttempts) {
        clearInterval(timer);
        console.debug(
          "addTrackToNewPeer: gave up waiting for WebRTC PC for",
          remotePeerId.toString(),
        );
      }
      return;
    }

    // Check we haven't already added our track to this PC.
    const alreadySending = _senders.some((s) => s.pc === pc);
    if (alreadySending) {
      clearInterval(timer);
      return;
    }

    // Don't add tracks while negotiation or post-glare cooldown is active.
    if (_negotiationBusy.has(pc)) {
      console.debug(
        "addTrackToNewPeer: skipping — negotiation in flight for",
        remotePeerId.toString().slice(-8),
      );
      // Don't clear interval — retry on next poll.
      return;
    }

    // Don't add tracks if the PC isn't in stable signaling state.
    if (pc.signalingState !== "stable") {
      console.debug(
        `addTrackToNewPeer: skipping — signalingState=${pc.signalingState} for`,
        remotePeerId.toString().slice(-8),
      );
      // Don't clear interval — retry on next poll.
      return;
    }

    clearInterval(timer);
    attachPCHandlers(pc);
    try {
      const sender = pc.addTrack(track, _localStream);
      _senders.push({ pc, sender, peerId: remotePeerId });
      console.log(
        "Added audio track to newly connected peer",
        remotePeerId.toString(),
      );
      // addTrack will fire negotiationneeded, which triggers the
      // offer/answer exchange automatically.
    } catch (err) {
      console.warn(
        "Failed to add track to new peer",
        remotePeerId.toString(),
        err,
      );
    }
  }, intervalMs);
}

// Get or create a hidden <audio> element for a specific peer connection.
function ensureRemoteAudioFor(pc) {
  if (_remoteAudios.has(pc)) return _remoteAudios.get(pc);
  const audio = document.createElement("audio");
  audio.autoplay = true;
  audio.muted = !_audioJoined;
  audio.style.display = "none";
  document.body.appendChild(audio);
  _remoteAudios.set(pc, audio);
  return audio;
}

// Remove the audio element for a peer connection that has closed.
function removeRemoteAudioFor(pc) {
  const audio = _remoteAudios.get(pc);
  if (audio) {
    audio.pause();
    audio.srcObject = null;
    audio.remove();
    _remoteAudios.delete(pc);
  }
}

// Read a full message from a libp2p v3 stream.
// The async iterator may yield close/reset events instead of data when the
// remote peer terminates the stream early — skip anything that isn't a
// Uint8Array to avoid propagating event objects as errors.
async function readStream(stream) {
  const chunks = [];
  try {
    for await (const chunk of stream) {
      // libp2p streams yield Uint8Array-like objects (BufferList slices).
      // Skip anything that isn't actual data (e.g. close events).
      if (chunk == null || typeof chunk.subarray !== "function") continue;
      chunks.push(chunk.subarray());
    }
  } catch (err) {
    // Stream may have been reset/closed mid-read.  If we already collected
    // some data, try to use it.  Otherwise, re-throw.
    if (chunks.length === 0) throw err;
  }
  const bytes = new Uint8Array(
    chunks.reduce((acc, c) => acc + c.length, 0),
  );
  let offset = 0;
  for (const c of chunks) {
    bytes.set(c, offset);
    offset += c.length;
  }
  return new TextDecoder().decode(bytes);
}

// Send a signaling message to a peer via the libp2p signaling protocol.
// Retries up to 3 times with exponential back-off (1s, 2s, 4s) when the
// underlying stream dial times out or fails transiently.
async function sendSignalingMessage(peerId, message) {
  const encoded = new TextEncoder().encode(JSON.stringify(message));
  const maxAttempts = 3;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    try {
      const stream = await _libp2p.dialProtocol(peerId, SIGNALING_PROTOCOL);
      stream.send(encoded);
      await stream.close();
      return;
    } catch (err) {
      if (attempt === maxAttempts) throw err;
      const delayMs = 1000 * Math.pow(2, attempt - 1);
      console.debug(
        `sendSignalingMessage attempt ${attempt} failed, retrying in ${delayMs}ms:`,
        err.message,
      );
      await new Promise((r) => setTimeout(r, delayMs));
    }
  }
}

// Attach negotiationneeded + track listeners to a peer connection.
// Safe to call multiple times — will only attach once per PC.
function attachPCHandlers(pc) {
  if (_attachedPCs.has(pc)) return;
  _attachedPCs.add(pc);

  const peerLabel = () => {
    const pid = _pcToPeer.get(pc);
    return pid ? pid.toString().slice(-8) : "unknown";
  };

  // --- Detailed WebRTC lifecycle logging ---
  pc.addEventListener("iceconnectionstatechange", () => {
    console.log(
      `[WebRTC ${peerLabel()}] ICE connection state: ${pc.iceConnectionState}`,
    );
  });

  pc.addEventListener("icegatheringstatechange", () => {
    console.log(
      `[WebRTC ${peerLabel()}] ICE gathering state: ${pc.iceGatheringState}`,
    );
  });

  pc.addEventListener("connectionstatechange", () => {
    console.log(
      `[WebRTC ${peerLabel()}] Connection state: ${pc.connectionState}`,
    );
  });

  pc.addEventListener("signalingstatechange", () => {
    console.log(
      `[WebRTC ${peerLabel()}] Signaling state: ${pc.signalingState}`,
    );
  });

  pc.addEventListener("icecandidate", (event) => {
    if (event.candidate) {
      console.debug(
        `[WebRTC ${peerLabel()}] ICE candidate: ${event.candidate.type ?? "unknown"} ${event.candidate.protocol ?? ""} ${event.candidate.address ?? ""}:${event.candidate.port ?? ""}`,
      );
    } else {
      console.debug(`[WebRTC ${peerLabel()}] ICE gathering complete`);
    }
  });

  // When addTrack() or removeTrack() changes the SDP, create and send an offer.
  pc.addEventListener("negotiationneeded", async () => {
    const peerId = _pcToPeer.get(pc);
    if (!peerId) {
      console.warn("negotiationneeded fired but no peer ID mapped for PC");
      return;
    }
    // If this PC no longer backs a live libp2p connection (e.g. the
    // connection was downgraded to circuit relay), skip the renegotiation
    // — the offer would be sent over the relay and discarded.
    if (!isPCStillLive(pc)) {
      console.log(
        "Skipping negotiationneeded — PC is no longer live for",
        peerId.toString(),
      );
      return;
    }
    // Skip if we're already mid-negotiation on this PC to avoid glare.
    if (_negotiationBusy.has(pc)) {
      console.log("Skipping negotiationneeded — negotiation already in flight");
      return;
    }
    _negotiationBusy.add(pc);
    try {
      // Request ICE restart if the connection is not in a healthy state,
      // so the offer's ice-ufrag/ice-pwd change is intentional.
      const iceState = pc.iceConnectionState;
      const needsRestart =
        iceState === "disconnected" ||
        iceState === "failed" ||
        iceState === "closed";
      const offer = await pc.createOffer({
        iceRestart: needsRestart,
      });
      await pc.setLocalDescription(offer);
      await sendSignalingMessage(peerId, {
        type: "offer",
        sdp: pc.localDescription.sdp,
      });
      console.log(
        "Sent renegotiation offer to",
        peerId.toString(),
        needsRestart ? "(with ICE restart)" : "",
      );
    } catch (err) {
      console.error("Failed to send renegotiation offer:", err);
    } finally {
      _negotiationBusy.delete(pc);
    }
  });

  // When a remote peer adds a track, play it on a per-peer audio element.
  pc.addEventListener("track", (event) => {
    console.log(
      `[WebRTC ${peerLabel()}] Track event: kind=${event.track.kind} state=${event.track.readyState} streams=${event.streams.length}`,
    );
    const audio = ensureRemoteAudioFor(pc);
    if (event.streams && event.streams.length > 0) {
      audio.srcObject = event.streams[0];
    } else {
      const stream = new MediaStream([event.track]);
      audio.srcObject = stream;
    }
    // Ensure playback if user has joined audio.
    if (_audioJoined) {
      audio.muted = false;
      audio.play().catch(() => {});
    }
  });

  // Clean up the audio element when the peer connection closes.
  pc.addEventListener("connectionstatechange", () => {
    if (pc.connectionState === "closed" || pc.connectionState === "failed") {
      removeRemoteAudioFor(pc);
    }
  });
}

// Register the signaling protocol handler on the libp2p node.
// Handles incoming SDP offers and answers from remote peers.
export function register_signaling_handler() {
  if (!_libp2p) return;
  _libp2p.handle(SIGNALING_PROTOCOL, async (stream, connection) => {
    try {
      const text = await readStream(stream);
      const message = JSON.parse(text);
      const remotePeerId = connection.remotePeer; // PeerId object

      // Find the RTCPeerConnection for this peer.
      // First try the connection the message arrived on; if that's a relay
      // connection (no PC), scan all connections for a WebRTC one to the
      // same peer.
      let pc = getPeerConnection(connection);
      if (!pc) {
        pc = findPCForPeer(remotePeerId);
      }
      if (!pc) {
        console.warn(
          "Signaling message but no WebRTC connection for peer:",
          remotePeerId.toString(),
        );
        return;
      }
      _pcToPeer.set(pc, remotePeerId);
      attachPCHandlers(pc);

      if (message.type === "offer") {
        // Try to apply the offer to the primary PC.  If it fails with an
        // ICE-credential mismatch, the peer may have multiple WebRTC
        // connections — try each one.  This happens when both sides dial
        // simultaneously and libp2p creates 2+ PCs per peer.
        const applied = await tryApplyOffer(pc, remotePeerId, message.sdp);
        if (!applied) {
          console.error(
            "Failed to apply offer on any PC for",
            remotePeerId.toString(),
          );
          return;
        }
      } else if (message.type === "answer") {
        // Only apply the answer if we're actually expecting one.
        // Try the primary PC first, then others if it doesn't match.
        // Wrap in try/catch: after glare resolution the answer may be
        // for a rolled-back offer whose SDP no longer matches.
        let answered = false;

        if (pc.signalingState === "have-local-offer" &&
            pc.connectionState !== "closed" && pc.connectionState !== "failed") {
          try {
            await pc.setRemoteDescription({
              type: "answer",
              sdp: message.sdp,
            });
            answered = true;
          } catch (err) {
            console.warn(
              `Failed to apply answer on primary PC for ${remotePeerId.toString().slice(-8)} (SDP mismatch?):`,
              err.message,
            );
            // The PC is now stuck in have-local-offer with no valid answer
            // coming.  Roll back to stable so it can accept new offers or
            // create fresh ones via negotiationneeded.
            try {
              await pc.setLocalDescription({ type: "rollback" });
              console.log(
                `Rolled back stuck have-local-offer on primary PC [${remotePeerId.toString().slice(-8)}]`,
              );
              schedulePostGlareRenegotiation(pc);
            } catch (rollbackErr) {
              console.warn(
                `Rollback after answer failure also failed [${remotePeerId.toString().slice(-8)}]:`,
                rollbackErr.message,
              );
            }
          }
        }

        if (!answered) {
          // The answer may be for a different PC (multi-connection case).
          const allPCs = findAllPCsForPeer(remotePeerId);
          for (const otherPC of allPCs) {
            if (otherPC === pc) continue;
            if (otherPC.connectionState === "closed" || otherPC.connectionState === "failed") continue;
            if (otherPC.signalingState === "have-local-offer") {
              try {
                await otherPC.setRemoteDescription({
                  type: "answer",
                  sdp: message.sdp,
                });
                answered = true;
                break;
              } catch (answerErr) {
                // Answer didn't match this PC either — roll back so it
                // doesn't stay stuck in have-local-offer.
                try {
                  await otherPC.setLocalDescription({ type: "rollback" });
                  console.log(
                    `Rolled back stuck have-local-offer on alt PC [${remotePeerId.toString().slice(-8)}]`,
                  );
                  schedulePostGlareRenegotiation(otherPC);
                } catch (_) {
                  // Best effort
                }
              }
            }
          }
        }

        if (!answered) {
          // This is expected after glare resolution — the answer is for
          // the offer that was rolled back, and can be safely ignored.
          console.debug(
            "Ignoring stale answer — no PC in have-local-offer state for",
            remotePeerId.toString(),
            "(likely already handled by glare resolution)",
          );
        }
      } else {
        console.warn("Unknown signaling message type:", message.type);
      }
    } catch (err) {
      console.error("Signaling handler error:", err);
    }
  });
}

// Try to apply a remote SDP offer to a peer connection.  If the primary PC
// fails due to ICE credential mismatch (multiple connections), try all other
// PCs for the same peer.  Returns true if successfully applied + answered.
async function tryApplyOffer(primaryPC, remotePeerId, sdp) {
  // Build ordered list: primary first, then alternatives.
  const allPCs = findAllPCsForPeer(remotePeerId);
  const ordered = [primaryPC, ...allPCs.filter((p) => p !== primaryPC)];

  for (const pc of ordered) {
    // Skip PCs whose underlying transport is already dead.  This prevents
    // wasted SDP negotiation on connections killed by libp2p's duplicate
    // multiaddr abort (connected -> closed with no disconnected phase).
    if (pc.connectionState === "closed" || pc.connectionState === "failed") {
      console.debug(
        `Skipping dead PC (${pc.connectionState}) in tryApplyOffer [${remotePeerId.toString().slice(-8)}]`,
      );
      continue;
    }

    _pcToPeer.set(pc, remotePeerId);
    attachPCHandlers(pc);

    try {
      // If we're in the middle of our own offer (glare), roll back first.
      let wasGlare = false;
      if (pc.signalingState === "have-local-offer") {
        console.log(
          `Rolling back local offer on PC to accept remote offer (glare) [${remotePeerId.toString().slice(-8)}]`,
        );
        await pc.setLocalDescription({ type: "rollback" });
        wasGlare = true;
      }

      await pc.setRemoteDescription({ type: "offer", sdp });

      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);

      // IMPORTANT: Set _negotiationBusy BEFORE sendSignalingMessage.
      // setLocalDescription(answer) can synchronously fire negotiationneeded
      // (e.g. if addTrack still needs SDP negotiation after glare rollback).
      // If we wait until after sendSignalingMessage, the negotiationneeded
      // handler can run during the await and bypass the cooldown.
      if (wasGlare) {
        _negotiationBusy.add(pc);
      }

      await sendSignalingMessage(remotePeerId, {
        type: "answer",
        sdp: pc.localDescription.sdp,
      });

      // After glare resolution, schedule a deferred renegotiation so the
      // rolled-back addTrack gets a fresh offer after the cooldown.
      if (wasGlare) {
        schedulePostGlareRenegotiation(pc);
      } else if (_negotiationBusy.has(pc)) {
        // We accepted a new offer while already in a post-glare cooldown.
        // Reset the cooldown timer so the deferred renegotiation doesn't
        // fire too soon after this exchange.
        schedulePostGlareRenegotiation(pc);
      }

      return true;
    } catch (err) {
      const isIceRestart =
        err instanceof DOMException &&
        (err.message.includes("ICE restart") ||
         err.message.includes("ice-ufrag") ||
         err.message.includes("ice-pwd"));

      if (isIceRestart) {
        // This PC has mismatched ICE credentials — try advancing ICE
        // generation before giving up on this PC.
        try {
          const localOffer = await pc.createOffer({ iceRestart: true });
          await pc.setLocalDescription(localOffer);
          await pc.setLocalDescription({ type: "rollback" });
          await pc.setRemoteDescription({ type: "offer", sdp });

          const answer = await pc.createAnswer();
          await pc.setLocalDescription(answer);

          // Set _negotiationBusy BEFORE sendSignalingMessage (same race fix
          // as the normal glare path above).
          _negotiationBusy.add(pc);

          await sendSignalingMessage(remotePeerId, {
            type: "answer",
            sdp: pc.localDescription.sdp,
          });
          console.log(
            `Applied offer after ICE restart recovery [${remotePeerId.toString().slice(-8)}]`,
          );

          // ICE restart recovery also involves a rollback — schedule
          // cooldown for pending local tracks.
          schedulePostGlareRenegotiation(pc);

          return true;
        } catch (retryErr) {
          console.debug(
            `ICE restart recovery failed on PC, trying next [${remotePeerId.toString().slice(-8)}]:`,
            retryErr.message,
          );
          // Fall through to try next PC
        }
      } else {
        console.debug(
          `setRemoteDescription failed on PC, trying next [${remotePeerId.toString().slice(-8)}]:`,
          err.message,
        );
        // Fall through to try next PC
      }
    }
  }
  return false;
}

// Start sending microphone audio to all connected WebRTC peers.
// Calls on_ok() on success, on_error(msg) on failure.
export function start_audio(on_ok, on_error) {
  if (_localStream) {
    on_error("Audio already active");
    return;
  }
  const peers = getWebRTCPeers();
  if (peers.length === 0) {
    on_error("No WebRTC peers connected");
    return;
  }
  navigator.mediaDevices
    .getUserMedia({ audio: true, video: false })
    .then((stream) => {
      _localStream = stream;
      _senders = [];
      const track = stream.getAudioTracks()[0];
      for (const { peerId, pc } of peers) {
        attachPCHandlers(pc);
        try {
          const sender = pc.addTrack(track, stream);
          _senders.push({ pc, sender, peerId });
        } catch (err) {
          console.warn(`Failed to add track to ${peerId.toString()}:`, err);
        }
      }
      // negotiationneeded will fire asynchronously, triggering the
      // offer/answer exchange via the signaling protocol.
      on_ok();
    })
    .catch((err) => {
      on_error(err.toString());
    });
}

// Stop sending audio and clean up.
export function stop_audio() {
  if (_localStream) {
    for (const { pc, sender } of _senders) {
      try {
        pc.removeTrack(sender);
      } catch (_) {
        // peer connection may already be closed
      }
    }
    _senders = [];
    // Stop all local tracks (releases microphone)
    for (const track of _localStream.getTracks()) {
      track.stop();
    }
    _localStream = null;
    // removeTrack triggers negotiationneeded, which will renegotiate
    // to remove the audio from the SDP automatically.
  }
}

// Returns true if we are currently sending audio.
export function is_audio_active() {
  return _localStream != null;
}

// Returns true if we are receiving remote audio from any peer.
export function is_receiving_audio() {
  for (const audio of _remoteAudios.values()) {
    if (!audio.srcObject) continue;
    const tracks = audio.srcObject.getAudioTracks();
    if (tracks.some((t) => t.readyState === "live")) return true;
  }
  return false;
}

// Join audio listening: unmute all remote audio elements so the user
// can hear incoming streams.
export function join_audio_listening() {
  _audioJoined = true;
  for (const audio of _remoteAudios.values()) {
    audio.muted = false;
    audio.autoplay = true;
    if (audio.srcObject) {
      audio.play().catch(() => {});
    }
  }
}

// Leave audio listening: mute all remote audio elements and pause playback.
export function leave_audio_listening() {
  _audioJoined = false;
  for (const audio of _remoteAudios.values()) {
    audio.muted = true;
    audio.pause();
  }
}

// Returns true if the user has joined audio listening.
export function is_audio_joined() {
  return _audioJoined;
}

// -- Audio presence --
//
// Lightweight protocol to tell peers whether we've joined audio and
// whether our mic is muted.  Each message is a small JSON object:
//   { "joined": bool, "muted": bool }
// We send our state to every WebRTC peer whenever it changes and
// periodically (called from the Gleam Tick) so that newly connected
// peers learn our state quickly.

const AUDIO_PRESENCE_PROTOCOL = "/sunset/audio-presence/1.0.0";
const _peerAudioStates = new Map(); // peer ID string -> { joined, muted }

// Register the handler that receives audio presence from remote peers.
export function register_audio_presence_handler() {
  if (!_libp2p) return;
  _libp2p.handle(AUDIO_PRESENCE_PROTOCOL, async (stream, connection) => {
    try {
      const text = await readStream(stream);
      const message = JSON.parse(text);
      const remotePeerId = connection.remotePeer.toString();
      _peerAudioStates.set(remotePeerId, {
        joined: !!message.joined,
        muted: !!message.muted,
      });
    } catch (err) {
      console.debug("Audio presence handler error:", err.message);
    }
  });
}

// Broadcast our current audio state to all WebRTC peers.
// Called on every Tick and whenever local audio state changes.
export function broadcast_audio_presence() {
  if (!_libp2p) return;
  const message = {
    joined: _audioJoined,
    muted: _audioJoined && !_localStream,
  };
  const encoded = new TextEncoder().encode(JSON.stringify(message));
  for (const { peerId } of getWebRTCPeers()) {
    _libp2p
      .dialProtocol(peerId, AUDIO_PRESENCE_PROTOCOL)
      .then((stream) => {
        stream.send(encoded);
        return stream.close();
      })
      .catch(() => {
        // Peer may not support the protocol yet — ignore silently.
      });
  }
}

// Return the audio presence states of all peers as a Gleam-friendly
// List of [peer_id, joined_string, muted_string].
export function get_peer_audio_states() {
  // Clean up entries for peers that are no longer connected.
  if (_libp2p) {
    const connectedIds = new Set(_libp2p.getPeers().map((p) => p.toString()));
    for (const pid of _peerAudioStates.keys()) {
      if (!connectedIds.has(pid)) _peerAudioStates.delete(pid);
    }
  }
  const results = [];
  for (const [pid, state] of _peerAudioStates) {
    results.push(toList([pid, state.joined ? "true" : "false", state.muted ? "true" : "false"]));
  }
  return toList(results);
}

// Migrate audio tracks to new PCs when connections are re-established.
// Called after successful WebRTC reconnection and periodically from Tick.
// For each peer we're sending audio to, ensure the track is on the current
// (live) PC, not a stale one.  Also cleans up _senders for dead PCs.
export function migrate_audio_tracks() {
  if (!_localStream) return;
  const track = _localStream.getAudioTracks()[0];
  if (!track) return;

  // Remove senders for PCs that are no longer live.
  const staleSenders = _senders.filter((s) => !isPCStillLive(s.pc));
  for (const stale of staleSenders) {
    try {
      stale.pc.removeTrack(stale.sender);
    } catch (_) {
      // PC may already be closed
    }
    console.log(
      `[AudioMigrate] Removed stale sender for ${stale.peerId.toString().slice(-8)}`,
    );
  }
  _senders = _senders.filter((s) => isPCStillLive(s.pc));

  // For each WebRTC peer, ensure we have a sender on their current PC.
  const peers = getWebRTCPeers();
  for (const { peerId, pc } of peers) {
    const hasSender = _senders.some((s) => s.pc === pc);
    if (hasSender) continue;

    // Don't add tracks while a negotiation or post-glare cooldown is active
    // on this PC — it will fire negotiationneeded which will either be
    // suppressed (wasting the event) or cause a conflicting offer.
    if (_negotiationBusy.has(pc)) {
      console.debug(
        `[AudioMigrate] Skipping addTrack for ${peerId.toString().slice(-8)} — negotiation in flight`,
      );
      continue;
    }

    // Don't add tracks if the PC isn't in stable signaling state — it means
    // an offer/answer exchange is already underway.
    if (pc.signalingState !== "stable") {
      console.debug(
        `[AudioMigrate] Skipping addTrack for ${peerId.toString().slice(-8)} — signalingState=${pc.signalingState}`,
      );
      continue;
    }

    attachPCHandlers(pc);
    try {
      const sender = pc.addTrack(track, _localStream);
      _senders.push({ pc, sender, peerId });
      console.log(
        `[AudioMigrate] Added audio track to new PC for ${peerId.toString().slice(-8)}`,
      );
      // addTrack fires negotiationneeded automatically.
    } catch (err) {
      console.warn(
        `[AudioMigrate] Failed to add track to ${peerId.toString().slice(-8)}:`,
        err,
      );
    }
  }
}

// -- WebRTC reconnection --
//
// When a WebRTC connection dies and falls back to circuit relay, we try to
// re-establish the direct WebRTC connection.  The relay-only state is detected
// by checking whether a peer has any WebRTC connection or only circuit relay.
// We attempt reconnection at most once every RECONNECT_COOLDOWN_MS per peer.
//
// To avoid both sides dialing simultaneously (which creates duplicate
// connections that libp2p immediately aborts), we use a deterministic
// tie-breaker: the peer with the lexicographically higher ID is the primary
// dialer.  The lower-ID peer waits 2× the cooldown before trying as fallback.

const RECONNECT_COOLDOWN_MS = 15_000;
const _reconnectLastAttempt = new Map(); // peer ID string -> timestamp
const _reconnectInFlight = new Set(); // peer ID strings currently being reconnected

// Check all connected peers and attempt to upgrade any relay-only connections
// to WebRTC.  Safe to call frequently (called every Tick) — cooldown prevents
// excessive attempts.
export function attempt_webrtc_reconnections() {
  if (!_libp2p) return;
  const now = Date.now();

  // Group connections by peer
  const peerConns = new Map(); // peer ID string -> { hasWebRTC, hasRelay, relayAddr }
  for (const conn of _libp2p.getConnections()) {
    const pid = conn.remotePeer.toString();
    const ma = conn.remoteAddr;
    const entry = peerConns.get(pid) || { hasWebRTC: false, hasRelay: false, relayAddr: null, peerId: conn.remotePeer };
    if (WebRTC.exactMatch(ma)) {
      entry.hasWebRTC = true;
    } else if (Circuit.exactMatch(ma)) {
      entry.hasRelay = true;
      entry.relayAddr = ma.toString();
    }
    peerConns.set(pid, entry);
  }

  for (const [pidStr, info] of peerConns) {
    // Only interested in peers with relay but no WebRTC
    if (info.hasWebRTC || !info.hasRelay) continue;

    // Skip the relay peer itself
    const relayPeerId = _getRelayPeerId();
    if (relayPeerId && pidStr === relayPeerId.toString()) continue;

    // Deterministic tie-breaker: only the peer with the higher ID dials.
    // The lower-ID peer uses 2× the cooldown as a fallback in case the
    // higher-ID peer fails to reconnect.
    const localId = _libp2p.peerId.toString();
    const isPrimaryDialer = localId > pidStr;
    const effectiveCooldown = isPrimaryDialer
      ? RECONNECT_COOLDOWN_MS
      : RECONNECT_COOLDOWN_MS * 2;

    // Cooldown check
    const lastAttempt = _reconnectLastAttempt.get(pidStr) || 0;
    if (now - lastAttempt < effectiveCooldown) continue;

    // Skip if already in flight
    if (_reconnectInFlight.has(pidStr)) continue;

    _reconnectLastAttempt.set(pidStr, now);
    _reconnectInFlight.add(pidStr);

    console.log(
      `[Reconnect] Peer ${pidStr.slice(-8)} has only circuit relay — attempting WebRTC upgrade (${isPrimaryDialer ? "primary" : "fallback"})`,
    );

    // Construct a /webrtc address through the relay to this peer.
    // Format: <relay-multiaddr>/p2p-circuit/webrtc/p2p/<target-peer-id>
    const relayId = relayPeerId ? relayPeerId.toString() : null;
    if (!relayId) {
      console.debug("[Reconnect] No relay peer, cannot construct WebRTC address");
      _reconnectInFlight.delete(pidStr);
      continue;
    }

    // Find the relay's websocket address
    let relayBaseAddr = null;
    for (const conn of _libp2p.getConnections()) {
      if (conn.remotePeer.toString() !== relayId) continue;
      const ma = conn.remoteAddr;
      if (WebSockets.exactMatch(ma) || WebSocketsSecure.exactMatch(ma) || WebTransport.exactMatch(ma)) {
        relayBaseAddr = ma.toString();
        break;
      }
    }

    if (!relayBaseAddr) {
      console.debug("[Reconnect] Cannot find relay base address");
      _reconnectInFlight.delete(pidStr);
      continue;
    }

    const webrtcAddr = `${relayBaseAddr}/p2p-circuit/webrtc/p2p/${pidStr}`;
    console.log(`[Reconnect] Dialing ${webrtcAddr}`);

    const ma = multiaddr(webrtcAddr);
    _libp2p
      .dial(ma)
      .then(() => {
        console.log(
          `[Reconnect] Successfully re-established WebRTC connection to ${pidStr.slice(-8)}`,
        );
        // Migrate audio tracks to the new WebRTC PC so audio flows
        // over the direct connection instead of the dead/relay path.
        migrate_audio_tracks();
      })
      .catch((err) => {
        console.warn(
          `[Reconnect] Failed to re-establish WebRTC to ${pidStr.slice(-8)}: ${err.message}`,
        );
      })
      .finally(() => {
        _reconnectInFlight.delete(pidStr);
      });
  }
}

// Return diagnostic info about connections for debugging.
// Returns a Gleam List of [peer_id, transport, state_info] entries.
export function get_connection_diagnostics() {
  if (!_libp2p) return toList([]);
  const results = [];
  for (const conn of _libp2p.getConnections()) {
    const pid = conn.remotePeer.toString();
    const ma = conn.remoteAddr;
    let transport = "Other";
    if (WebRTC.exactMatch(ma)) transport = "WebRTC";
    else if (Circuit.exactMatch(ma)) transport = "Circuit Relay";
    else if (WebSocketsSecure.exactMatch(ma)) transport = "WebSockets (secure)";
    else if (WebSockets.exactMatch(ma)) transport = "WebSockets";
    else if (WebTransport.exactMatch(ma)) transport = "WebTransport";

    const pc = getPeerConnection(conn);
    let stateInfo = "n/a";
    if (pc) {
      stateInfo = `conn=${pc.connectionState} ice=${pc.iceConnectionState} sig=${pc.signalingState}`;
    }

    results.push(toList([pid, transport, stateInfo]));
  }
  return toList(results);
}

// -- Room-based peer discovery via relay --
//
// The relay runs a custom request-response protocol (/sunset/discovery/1.0.0).
// We periodically open a stream, send our room + addresses, and receive back
// all other peers in that room. The relay uses libp2p-request-response with
// JSON codec, which frames messages as: <unsigned-varint-length><json-bytes>.

const DISCOVERY_PROTOCOL = "/sunset/discovery/1.0.0";
const DISCOVERY_POLL_MS = 2_000;

let _discoveryRoom = null;
let _discoveryInterval = null;
let _onPeerDiscovered = null;

// Encode an unsigned varint (used by libp2p length-prefixed framing).
function encodeUvarint(value) {
  const bytes = [];
  while (value >= 0x80) {
    bytes.push((value & 0x7f) | 0x80);
    value >>>= 7;
  }
  bytes.push(value & 0x7f);
  return new Uint8Array(bytes);
}

// Decode an unsigned varint from a Uint8Array, returning [value, bytesRead].
function decodeUvarint(buf, offset = 0) {
  let value = 0;
  let shift = 0;
  let i = offset;
  while (i < buf.length) {
    const byte = buf[i];
    value |= (byte & 0x7f) << shift;
    i++;
    if ((byte & 0x80) === 0) return [value, i - offset];
    shift += 7;
    if (shift > 35) throw new Error("varint too long");
  }
  throw new Error("varint incomplete");
}

// Write a length-prefixed JSON message to a libp2p stream.
function writeLengthPrefixed(stream, obj) {
  const json = new TextEncoder().encode(JSON.stringify(obj));
  const lenBytes = encodeUvarint(json.length);
  const frame = new Uint8Array(lenBytes.length + json.length);
  frame.set(lenBytes, 0);
  frame.set(json, lenBytes.length);
  stream.send(frame);
}

// Read a full length-prefixed JSON message from a libp2p stream.
async function readLengthPrefixed(stream) {
  const chunks = [];
  for await (const chunk of stream) {
    chunks.push(chunk.subarray());
  }
  if (chunks.length === 0) throw new Error("Empty response");
  const buf = new Uint8Array(chunks.reduce((a, c) => a + c.length, 0));
  let offset = 0;
  for (const c of chunks) {
    buf.set(c, offset);
    offset += c.length;
  }
  const [len, varintSize] = decodeUvarint(buf);
  const json = new TextDecoder().decode(buf.slice(varintSize, varintSize + len));
  return JSON.parse(json);
}

// Read raw JSON from a libp2p stream until EOF (remote half-close).
// Used with rust-libp2p request_response::json codec which has no length-prefix.
async function readRawJson(stream) {
  const chunks = [];
  for await (const chunk of stream) {
    chunks.push(chunk.subarray());
  }
  if (chunks.length === 0) throw new Error("Empty response");
  const buf = new Uint8Array(chunks.reduce((a, c) => a + c.length, 0));
  let offset = 0;
  for (const c of chunks) {
    buf.set(c, offset);
    offset += c.length;
  }
  return JSON.parse(new TextDecoder().decode(buf));
}

// Subscribe to a room for peer discovery. Polls the relay periodically.
// on_discovered(peer_id, addrs_gleam_list) is called for each discovered peer.
export function subscribe_to_room(room_name, on_discovered) {
  if (!_libp2p) return;

  // Unsubscribe first if already subscribed
  unsubscribe_from_room();

  _discoveryRoom = room_name;
  _onPeerDiscovered = on_discovered;

  // Poll immediately, then on interval
  _pollDiscovery();
  _discoveryInterval = setInterval(_pollDiscovery, DISCOVERY_POLL_MS);
}

// Unsubscribe from room discovery.
export function unsubscribe_from_room() {
  if (_discoveryInterval) {
    clearInterval(_discoveryInterval);
    _discoveryInterval = null;
  }
  _discoveryRoom = null;
  _onPeerDiscovered = null;
}

// Send a discovery request to the relay and process the response.
async function _pollDiscovery() {
  if (!_libp2p || !_discoveryRoom) return;

  const relayPeerId = _getRelayPeerId();
  if (!relayPeerId) {
    console.debug("Discovery poll: no relay connection yet");
    return;
  }

  try {
    const addrs = _libp2p.getMultiaddrs().map((ma) => ma.toString());
    const request = {
      room: _discoveryRoom,
      peer_id: _libp2p.peerId.toString(),
      addrs,
    };

    const stream = await _libp2p.dialProtocol(relayPeerId, DISCOVERY_PROTOCOL);
    // rust-libp2p request_response::json codec uses raw JSON + read-to-EOF
    // (no length-prefix framing). Half-close signals end of request.
    const json = new TextEncoder().encode(JSON.stringify(request));
    stream.send(json);
    await stream.close();

    const response = await readRawJson(stream);

    if (response.peers && _onPeerDiscovered) {
      for (const peer of response.peers) {
        if (peer.peer_id !== _libp2p.peerId.toString()) {
          _onPeerDiscovered(peer.peer_id, toList(peer.addrs));
        }
      }
    }
  } catch (err) {
    console.debug("Discovery poll failed:", err.message);
  }
}

// Get the relay peer ID as a string (or "" if not connected).
export function get_relay_peer_id() {
  const p = _getRelayPeerId();
  return p ? p.toString() : "";
}

// Get the remote multiaddr for each connected peer.
// When a peer has multiple connections, prefers the direct (non-circuit) one.
// Returns a Gleam List of [peer_id, remote_addr] pairs (each a Gleam List of strings).
export function get_peer_remote_addrs() {
  if (!_libp2p) return toList([]);
  const best = new Map(); // peer_id -> { addr, transport, isCircuit }
  for (const conn of _libp2p.getConnections()) {
    const pid = conn.remotePeer.toString();
    const addr = conn.remoteAddr.toString();
    const ma = conn.remoteAddr;
    const isCircuit = Circuit.exactMatch(ma);
    let transport = "Other";
    if (WebRTC.exactMatch(ma)) transport = "WebRTC";
    else if (WebRTCDirect.exactMatch(ma)) transport = "WebRTC Direct";
    else if (WebSocketsSecure.exactMatch(ma)) transport = "WebSockets (secure)";
    else if (WebSockets.exactMatch(ma)) transport = "WebSockets";
    else if (WebTransport.exactMatch(ma)) transport = "WebTransport";
    else if (isCircuit) transport = "Circuit Relay";
    const existing = best.get(pid);
    if (!existing || (existing.isCircuit && !isCircuit)) {
      best.set(pid, { addr, transport, isCircuit });
    }
  }
  const results = [];
  for (const [pid, { addr, transport }] of best) {
    results.push(toList([pid, addr, transport]));
  }
  return toList(results);
}

// Get all connection addresses for a specific peer.
// Returns a Gleam List of [transport, remote_addr] pairs (each a Gleam List of strings).
export function get_peer_addrs(peer_id_str) {
  if (!_libp2p) return toList([]);
  const results = [];
  for (const conn of _libp2p.getConnections()) {
    if (conn.remotePeer.toString() !== peer_id_str) continue;
    const ma = conn.remoteAddr;
    let transport = "Other";
    if (WebRTC.exactMatch(ma)) transport = "WebRTC";
    else if (WebRTCDirect.exactMatch(ma)) transport = "WebRTC Direct";
    else if (WebSocketsSecure.exactMatch(ma)) transport = "WebSockets (secure)";
    else if (WebSockets.exactMatch(ma)) transport = "WebSockets";
    else if (WebTransport.exactMatch(ma)) transport = "WebTransport";
    else if (Circuit.exactMatch(ma)) transport = "Circuit Relay";
    results.push(toList([transport, ma.toString()]));
  }
  return toList(results);
}

// Get the PeerId of the connected relay (first peer that has a non-WebRTC connection).
function _getRelayPeerId() {
  if (!_libp2p) return null;
  for (const conn of _libp2p.getConnections()) {
    const ma = conn.remoteAddr;
    // The relay connection is via WebSocket (not WebRTC/circuit)
    if (
      WebSockets.exactMatch(ma) ||
      WebSocketsSecure.exactMatch(ma) ||
      WebTransport.exactMatch(ma)
    ) {
      return conn.remotePeer;
    }
  }
  return null;
}

// Dial a peer given a list of multiaddr strings. Tries each address
// sequentially until one succeeds, preferring direct WebRTC addresses
// over circuit relay. Calls on_ok() on first success,
// on_error(msg) if all fail.
export function dial_peer_addrs(addrs_list, on_ok, on_error) {
  if (!_libp2p) {
    on_error("libp2p not initialised");
    return;
  }

  // Convert Gleam list to JS array
  const addrs = [];
  let cursor = addrs_list;
  while (cursor.head !== undefined) {
    addrs.push(cursor.head);
    cursor = cursor.tail;
  }

  if (addrs.length === 0) {
    on_error("No addresses to dial");
    return;
  }

  // Sort: prefer direct WebRTC over circuit relay
  addrs.sort((a, b) => {
    const aCircuit = a.includes("/p2p-circuit");
    const bCircuit = b.includes("/p2p-circuit");
    if (aCircuit === bCircuit) return 0;
    return aCircuit ? 1 : -1;
  });

  (async () => {
    const errors = [];
    for (const addr of addrs) {
      try {
        const ma = multiaddr(addr);
        await _libp2p.dial(ma);
        on_ok();
        return;
      } catch (err) {
        errors.push(`${addr}: ${err.message}`);
      }
    }
    on_error("All addresses failed: " + errors.join("; "));
  })();
}
