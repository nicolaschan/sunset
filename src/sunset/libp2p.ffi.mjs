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

// -- Recently-disconnected peer tracking --
// When a peer disconnects from libp2p, we keep them in this map for a grace
// period so the UI can show them with a "disconnected" indicator (red dot)
// instead of instantly removing them. If they reconnect within the window
// we remove them from this map.
const _recentlyDisconnectedPeers = new Map(); // peer ID string -> { disconnectedAt: number }
const PEER_DISCONNECT_GRACE_MS = 30_000; // keep showing for 30 seconds

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
        const remotePeerId = event.detail;
        const remotePeerIdStr = remotePeerId.toString();
        console.log("peer:connect", remotePeerIdStr);
        // Peer came back — remove from recently-disconnected
        _recentlyDisconnectedPeers.delete(remotePeerIdStr);
        // If we have audio active, create a PC for the new peer
        if (_localStream && !_audioPCs.has(remotePeerIdStr)) {
          const relayPeerId = _getRelayPeerId();
          if (!relayPeerId || relayPeerId.toString() !== remotePeerIdStr) {
            const localId = _libp2p.peerId.toString();
            const shouldOffer = localId > remotePeerIdStr;
            createAudioPC(remotePeerId, shouldOffer);
          }
        }
      });
      libp2p.addEventListener("peer:disconnect", (event) => {
        const remotePeerIdStr = event.detail.toString();
        console.log("peer:disconnect", remotePeerIdStr);
        // Track as recently disconnected so the UI can show a red dot
        // instead of instantly removing the peer from the list.
        // Don't track the relay — it has its own status indicator.
        const relayPeerId = _getRelayPeerId();
        const relayStr = relayPeerId ? relayPeerId.toString() : null;
        if (remotePeerIdStr !== relayStr) {
          _recentlyDisconnectedPeers.set(remotePeerIdStr, {
            disconnectedAt: Date.now(),
          });
        }
        // Audio PCs are independent of libp2p transport — don't close them here.
        // They manage their own lifecycle via ICE (connectionstatechange -> failed/closed).
      });

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

// -- Audio --

let _localStream = null;
const _remoteAudios = new Map(); // peer ID string -> HTMLAudioElement
let _audioJoined = false; // Whether user has opted in to hear remote audio

// -- Audio signaling + standalone WebRTC connections --
//
// Audio uses our own RTCPeerConnection objects, completely separate from
// libp2p's internal WebRTC transport.  libp2p streams are used only for
// signaling (SDP offer/answer + ICE candidate exchange).
//
// Deterministic offerer: the peer with the lexicographically higher ID
// always creates the offer.  This prevents glare (simultaneous offers).

const AUDIO_SIGNALING_PROTOCOL = "/sunset/audio-signaling/1.0.0";

const STUN_SERVERS = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

const _audioPCs = new Map(); // peer ID string -> RTCPeerConnection
const _audioReconnectTimers = new Map(); // peer ID string -> timeout ID
const _audioReconnectCounts = new Map(); // peer ID string -> number of attempts so far
const AUDIO_RECONNECT_MAX_ATTEMPTS = 5;
const AUDIO_RECONNECT_BASE_DELAY_MS = 1000; // 1s, 2s, 4s, 8s, 16s

// Send an audio signaling message to a peer via libp2p stream.
// Fire-and-forget: one stream per message.
async function sendAudioSignaling(remotePeerId, message) {
  const encoded = new TextEncoder().encode(JSON.stringify(message));
  try {
    const stream = await _libp2p.dialProtocol(
      remotePeerId,
      AUDIO_SIGNALING_PROTOCOL,
      { runOnLimitedConnection: true },
    );
    await stream.send(encoded);
    await stream.close();
  } catch (err) {
    console.warn(
      `[AudioSignaling] Send failed to ${remotePeerId.toString().slice(-8)}:`,
      err.message,
    );
  }
}

// Look up a PeerId object by its string representation.
function _findPeerId(peerIdStr) {
  if (!_libp2p) return null;
  for (const pid of _libp2p.getPeers()) {
    if (pid.toString() === peerIdStr) return pid;
  }
  return null;
}

// Create a standalone RTCPeerConnection for audio with a remote peer.
// remotePeerId: libp2p PeerId object
// shouldOffer: if true, creates and sends an SDP offer
function createAudioPC(remotePeerId, shouldOffer) {
  const peerIdStr = remotePeerId.toString();

  // If we already have a PC for this peer, close it first
  closeAudioPC(peerIdStr);

  const pc = new RTCPeerConnection({ iceServers: STUN_SERVERS });
  _audioPCs.set(peerIdStr, pc);

  _setupAudioPC(pc, remotePeerId, peerIdStr, shouldOffer);

  return pc;
}

// Set up event handlers, add tracks, and optionally create an offer
// for an RTCPeerConnection. Shared by createAudioPC and attemptAudioReconnect.
function _setupAudioPC(pc, remotePeerId, peerIdStr, shouldOffer) {
  // ICE candidate trickling — send each candidate as it's discovered
  pc.addEventListener("icecandidate", (event) => {
    if (event.candidate) {
      sendAudioSignaling(remotePeerId, {
        type: "candidate",
        candidate: event.candidate.toJSON(),
      });
    }
  });

  // Remote track handler — play received audio
  pc.addEventListener("track", (event) => {
    console.log(
      `[Audio ${peerIdStr.slice(-8)}] Remote track: kind=${event.track.kind}`,
    );
    const audio = ensureRemoteAudioFor(peerIdStr);
    if (event.streams && event.streams.length > 0) {
      audio.srcObject = event.streams[0];
    } else {
      audio.srcObject = new MediaStream([event.track]);
    }
    if (_audioJoined) {
      audio.muted = false;
      audio.play().catch(() => {});
    }
  });

  // Connection state logging + reconnection on failure.
  // Only the offerer (higher peer ID) drives reconnection attempts.
  // The answerer just cleans up its dead PC so that when the offerer
  // sends a new offer, the signaling handler will create a fresh PC.
  const isOfferer = shouldOffer;
  pc.addEventListener("connectionstatechange", () => {
    console.log(
      `[Audio ${peerIdStr.slice(-8)}] Connection: ${pc.connectionState}`,
    );
    if (pc.connectionState === "connected") {
      // Connection succeeded — reset reconnect state
      _audioReconnectCounts.delete(peerIdStr);
      const timer = _audioReconnectTimers.get(peerIdStr);
      if (timer) {
        clearTimeout(timer);
        _audioReconnectTimers.delete(peerIdStr);
      }
    } else if (pc.connectionState === "disconnected") {
      // ICE disconnected — may recover on its own.
      // Only the offerer schedules a reconnect as a backup.
      if (isOfferer) {
        scheduleAudioReconnect(peerIdStr);
      }
    } else if (pc.connectionState === "failed") {
      // ICE failed — clean up this PC.
      removeRemoteAudioFor(peerIdStr);
      _audioPCs.delete(peerIdStr);
      // Only the offerer drives reconnection. The answerer waits
      // for a new offer to arrive via the signaling handler.
      if (isOfferer) {
        scheduleAudioReconnect(peerIdStr);
      }
    } else if (pc.connectionState === "closed") {
      // Explicitly closed (e.g. bye) — clean up, no reconnect
      removeRemoteAudioFor(peerIdStr);
      _audioPCs.delete(peerIdStr);
    }
  });

  pc.addEventListener("iceconnectionstatechange", () => {
    console.log(
      `[Audio ${peerIdStr.slice(-8)}] ICE: ${pc.iceConnectionState}`,
    );
  });

  // Add local audio track if we have one (mic is active)
  if (_localStream) {
    const track = _localStream.getAudioTracks()[0];
    if (track) {
      pc.addTrack(track, _localStream);
    }
  }

  // If we're the offerer, create and send the SDP offer
  if (shouldOffer) {
    (async () => {
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        await sendAudioSignaling(remotePeerId, {
          type: "offer",
          sdp: pc.localDescription.sdp,
        });
        console.log(`[Audio ${peerIdStr.slice(-8)}] Sent offer`);
      } catch (err) {
        console.error(
          `[Audio ${peerIdStr.slice(-8)}] Failed to create offer:`,
          err,
        );
      }
    })();
  }
}

// Close and remove an audio PC for a peer.
// If `resetReconnect` is true, also cancel any pending reconnect attempts.
function closeAudioPC(peerIdStr, resetReconnect = true) {
  if (resetReconnect) {
    _audioReconnectCounts.delete(peerIdStr);
    const timer = _audioReconnectTimers.get(peerIdStr);
    if (timer) {
      clearTimeout(timer);
      _audioReconnectTimers.delete(peerIdStr);
    }
  }
  const pc = _audioPCs.get(peerIdStr);
  if (pc) {
    pc.close();
    _audioPCs.delete(peerIdStr);
    removeRemoteAudioFor(peerIdStr);
  }
}

// Schedule a reconnect attempt for a failed/disconnected audio PC.
// Uses exponential backoff: 1s, 2s, 4s, 8s, 16s then gives up.
function scheduleAudioReconnect(peerIdStr) {
  // Don't reconnect if we're not in audio mode
  if (!_audioJoined || !_localStream) return;

  const prevAttempts = _audioReconnectCounts.get(peerIdStr) || 0;
  const attempt = prevAttempts + 1;

  // Clear any existing timer for this peer
  const existingTimer = _audioReconnectTimers.get(peerIdStr);
  if (existingTimer) {
    clearTimeout(existingTimer);
    _audioReconnectTimers.delete(peerIdStr);
  }

  if (attempt > AUDIO_RECONNECT_MAX_ATTEMPTS) {
    console.warn(
      `[Audio ${peerIdStr.slice(-8)}] Giving up reconnect after ${AUDIO_RECONNECT_MAX_ATTEMPTS} attempts`,
    );
    _audioReconnectCounts.delete(peerIdStr);
    return;
  }

  _audioReconnectCounts.set(peerIdStr, attempt);

  const delay = AUDIO_RECONNECT_BASE_DELAY_MS * Math.pow(2, attempt - 1);
  console.log(
    `[Audio ${peerIdStr.slice(-8)}] Scheduling reconnect attempt ${attempt}/${AUDIO_RECONNECT_MAX_ATTEMPTS} in ${delay}ms`,
  );

  const timer = setTimeout(() => {
    _audioReconnectTimers.delete(peerIdStr);
    attemptAudioReconnect(peerIdStr);
  }, delay);

  _audioReconnectTimers.set(peerIdStr, timer);
}

// Cancel a pending audio reconnect for a peer (clears both timer and count).
function cancelAudioReconnect(peerIdStr) {
  _audioReconnectCounts.delete(peerIdStr);
  const timer = _audioReconnectTimers.get(peerIdStr);
  if (timer) {
    clearTimeout(timer);
    _audioReconnectTimers.delete(peerIdStr);
  }
}

// Attempt to reconnect an audio PC to a peer.
function attemptAudioReconnect(peerIdStr) {
  // Bail out if we've left audio
  if (!_audioJoined || !_localStream) return;

  // Check if the peer is still reachable via libp2p
  const remotePeerId = _findPeerId(peerIdStr);
  if (!remotePeerId) {
    console.log(
      `[Audio ${peerIdStr.slice(-8)}] Reconnect skipped: peer not connected via libp2p`,
    );
    return;
  }

  // If there's already a healthy PC, skip
  const existingPC = _audioPCs.get(peerIdStr);
  if (existingPC && (existingPC.connectionState === "connected" || existingPC.connectionState === "connecting")) {
    console.log(
      `[Audio ${peerIdStr.slice(-8)}] Reconnect skipped: PC already ${existingPC.connectionState}`,
    );
    return;
  }

  console.log(`[Audio ${peerIdStr.slice(-8)}] Attempting audio reconnect`);

  // Close the old PC without resetting reconnect counts so the backoff
  // continues to increase if this attempt also fails.
  const oldPC = _audioPCs.get(peerIdStr);
  if (oldPC) {
    oldPC.close();
    _audioPCs.delete(peerIdStr);
    removeRemoteAudioFor(peerIdStr);
  }

  const localId = _libp2p.peerId.toString();
  const shouldOffer = localId > peerIdStr;

  // Create fresh PC — use the internal constructor directly to avoid
  // closeAudioPC resetting our reconnect counters.
  const pc = new RTCPeerConnection({ iceServers: STUN_SERVERS });
  _audioPCs.set(peerIdStr, pc);

  // Re-use the same setup as createAudioPC
  _setupAudioPC(pc, remotePeerId, peerIdStr, shouldOffer);
}

// Register the audio signaling protocol handler.
// Handles: offer, answer, candidate (ICE trickle), bye (clean hangup).
export function register_audio_signaling_handler() {
  if (!_libp2p) return;
  _libp2p.handle(
    AUDIO_SIGNALING_PROTOCOL,
    async (stream, connection) => {
      try {
        const text = await readStream(stream);
        const message = JSON.parse(text);
        const remotePeerId = connection.remotePeer;
        const peerIdStr = remotePeerId.toString();

        if (message.type === "offer") {
          let pc = _audioPCs.get(peerIdStr);

          // Glare: both sides sent offers simultaneously.
          // The peer with the higher ID wins (keeps their offer).
          if (pc && pc.signalingState === "have-local-offer") {
            const localId = _libp2p.peerId.toString();
            if (localId > peerIdStr) {
              // We have priority — ignore the remote offer
              console.log(
                `[AudioSignaling] Glare: ignoring offer from ${peerIdStr.slice(-8)} (we have priority)`,
              );
              return;
            }
            // They have priority — close our PC and accept their offer
            console.log(
              `[AudioSignaling] Glare: accepting offer from ${peerIdStr.slice(-8)} (they have priority)`,
            );
            pc.close();
            _audioPCs.delete(peerIdStr);
            pc = null;
          }

          if (!pc) {
            pc = createAudioPC(remotePeerId, false);
          }

          await pc.setRemoteDescription({ type: "offer", sdp: message.sdp });
          const answer = await pc.createAnswer();
          await pc.setLocalDescription(answer);
          await sendAudioSignaling(remotePeerId, {
            type: "answer",
            sdp: pc.localDescription.sdp,
          });
          console.log(
            `[AudioSignaling] Sent answer to ${peerIdStr.slice(-8)}`,
          );
        } else if (message.type === "answer") {
          const pc = _audioPCs.get(peerIdStr);
          if (!pc) {
            console.debug(
              `[AudioSignaling] Answer but no PC for ${peerIdStr.slice(-8)}`,
            );
            return;
          }
          if (pc.signalingState !== "have-local-offer") {
            console.debug(
              `[AudioSignaling] Answer but state=${pc.signalingState} for ${peerIdStr.slice(-8)}`,
            );
            return;
          }
          await pc.setRemoteDescription({
            type: "answer",
            sdp: message.sdp,
          });
          console.log(
            `[AudioSignaling] Applied answer from ${peerIdStr.slice(-8)}`,
          );
        } else if (message.type === "candidate") {
          const pc = _audioPCs.get(peerIdStr);
          if (!pc) {
            console.debug(
              `[AudioSignaling] ICE candidate but no PC for ${peerIdStr.slice(-8)}`,
            );
            return;
          }
          try {
            await pc.addIceCandidate(new RTCIceCandidate(message.candidate));
          } catch (err) {
            console.debug(
              `[AudioSignaling] Failed to add ICE candidate:`,
              err.message,
            );
          }
        } else if (message.type === "bye") {
          console.log(
            `[AudioSignaling] Bye from ${peerIdStr.slice(-8)}`,
          );
          closeAudioPC(peerIdStr);
        } else {
          console.warn(
            `[AudioSignaling] Unknown message type: ${message.type}`,
          );
        }
      } catch (err) {
        console.error("[AudioSignaling] Handler error:", err);
      }
    },
    { runOnLimitedConnection: true },
  );
}

// Return the audio PC connection states for all peers as a Gleam-friendly
// List of [peer_id, connection_state].
// connection_state is one of: "new", "connecting", "connected", "disconnected", "failed", "closed"
export function get_audio_pc_states() {
  const results = [];
  for (const [pid, pc] of _audioPCs) {
    results.push(toList([pid, pc.connectionState]));
  }
  return toList(results);
}

// Get or create a hidden <audio> element for a remote peer.
function ensureRemoteAudioFor(peerId) {
  if (_remoteAudios.has(peerId)) return _remoteAudios.get(peerId);
  const audio = document.createElement("audio");
  audio.autoplay = true;
  audio.muted = !_audioJoined;
  audio.style.display = "none";
  document.body.appendChild(audio);
  _remoteAudios.set(peerId, audio);
  return audio;
}

// Remove the audio element for a peer that has disconnected.
function removeRemoteAudioFor(peerId) {
  const audio = _remoteAudios.get(peerId);
  if (audio) {
    audio.pause();
    audio.srcObject = null;
    audio.remove();
    _remoteAudios.delete(peerId);
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

// Start capturing microphone audio and create WebRTC peer connections
// for all currently connected peers.
// If already acquired (unmuting), just re-enable existing tracks.
// Calls on_ok() on success, on_error(msg) on failure.
export function start_audio(on_ok, on_error) {
  // Unmute: stream exists, just re-enable tracks
  if (_localStream) {
    for (const track of _localStream.getAudioTracks()) {
      track.enabled = true;
    }
    on_ok();
    return;
  }
  navigator.mediaDevices
    .getUserMedia({ audio: true, video: false })
    .then((stream) => {
      _localStream = stream;
      // Create audio PCs for all connected peers
      const relayPeerId = _getRelayPeerId();
      const relayStr = relayPeerId ? relayPeerId.toString() : null;
      const localId = _libp2p.peerId.toString();
      for (const peerId of _libp2p.getPeers()) {
        const pidStr = peerId.toString();
        if (pidStr === relayStr) continue;
        if (_audioPCs.has(pidStr)) continue;
        const shouldOffer = localId > pidStr;
        createAudioPC(peerId, shouldOffer);
      }
      on_ok();
    })
    .catch((err) => {
      on_error(err.toString());
    });
}

// Mute mic: disable local audio tracks but keep PCs alive for receiving.
export function stop_audio() {
  if (_localStream) {
    for (const track of _localStream.getAudioTracks()) {
      track.enabled = false;
    }
  }
}

// Full teardown: send bye to all audio peers, close all PCs, release mic.
// Called when leaving audio entirely (not just muting).
export function close_all_audio_pcs() {
  // Cancel all pending reconnects
  for (const timer of _audioReconnectTimers.values()) {
    clearTimeout(timer);
  }
  _audioReconnectTimers.clear();
  _audioReconnectCounts.clear();

  // Send bye to all audio peers and close PCs
  for (const [peerIdStr, pc] of _audioPCs) {
    const peerId = _findPeerId(peerIdStr);
    if (peerId) {
      sendAudioSignaling(peerId, { type: "bye" });
    }
    pc.close();
    removeRemoteAudioFor(peerIdStr);
  }
  _audioPCs.clear();
  // Release the microphone
  if (_localStream) {
    for (const track of _localStream.getTracks()) {
      track.stop();
    }
    _localStream = null;
  }
}

// Returns true if we are currently sending audio (mic enabled).
export function is_audio_active() {
  if (!_localStream) return false;
  const tracks = _localStream.getAudioTracks();
  return tracks.length > 0 && tracks[0].enabled;
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
// We send our state to every connected peer whenever it changes and
// periodically (called from the Gleam Tick) so that newly connected
// peers learn our state quickly.

const AUDIO_PRESENCE_PROTOCOL = "/sunset/audio-presence/1.0.0";
const _peerAudioStates = new Map(); // peer ID string -> { joined, muted, name }
let _localDisplayName = ""; // Set from Gleam when user saves a display name

// Set the local display name (called from Gleam).
export function set_display_name(name) {
  _localDisplayName = name || "";
}

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
        name: message.name || "",
      });
    } catch (err) {
      console.debug("Audio presence handler error:", err.message);
    }
  }, { runOnLimitedConnection: true });
}

// Broadcast our current audio state to all connected peers.
// Called on every Tick and whenever local audio state changes.
export function broadcast_audio_presence() {
  if (!_libp2p) return;
  const message = {
    joined: _audioJoined,
    muted: _audioJoined && !_localStream,
    name: _localDisplayName,
  };
  const encoded = new TextEncoder().encode(JSON.stringify(message));
  const relayPeerId = _getRelayPeerId();
  const relayStr = relayPeerId ? relayPeerId.toString() : null;
  for (const peerId of _libp2p.getPeers()) {
    if (peerId.toString() === relayStr) continue;
    _libp2p
      .dialProtocol(peerId, AUDIO_PRESENCE_PROTOCOL, { runOnLimitedConnection: true })
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
  // Clean up entries for peers that are no longer connected
  // (and not in the recently-disconnected grace period).
  if (_libp2p) {
    const connectedIds = new Set(_libp2p.getPeers().map((p) => p.toString()));
    for (const pid of _peerAudioStates.keys()) {
      if (!connectedIds.has(pid) && !_recentlyDisconnectedPeers.has(pid)) {
        _peerAudioStates.delete(pid);
      }
    }
  }
  const results = [];
  for (const [pid, state] of _peerAudioStates) {
    results.push(toList([pid, state.joined ? "true" : "false", state.muted ? "true" : "false"]));
  }
  return toList(results);
}

// Return display names received from peers as a Gleam-friendly
// List of [peer_id, name] pairs. Only includes peers with a non-empty name.
export function get_peer_names() {
  const results = [];
  for (const [pid, state] of _peerAudioStates) {
    if (state.name) {
      results.push(toList([pid, state.name]));
    }
  }
  return toList(results);
}

// Return peers that recently disconnected and are still within the grace
// period. Prunes expired entries. Returns a Gleam List of peer ID strings.
export function get_recently_disconnected_peers() {
  const now = Date.now();
  const results = [];
  for (const [pid, entry] of _recentlyDisconnectedPeers) {
    if (now - entry.disconnectedAt > PEER_DISCONNECT_GRACE_MS) {
      _recentlyDisconnectedPeers.delete(pid);
    } else {
      results.push(pid);
    }
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
// Returns a Gleam List of [peer_id, remote_addr, transport] entries.
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
