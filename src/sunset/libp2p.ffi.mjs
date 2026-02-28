import { createLibp2p } from "libp2p";
import { identify } from "@libp2p/identify";
import { noise } from "@chainsafe/libp2p-noise";
import { yamux } from "@chainsafe/libp2p-yamux";
import { multiaddr } from "@multiformats/multiaddr";
import { webSockets } from "@libp2p/websockets";
import { webTransport } from "@libp2p/webtransport";
import { webRTC } from "@libp2p/webrtc";
import { circuitRelayTransport } from "@libp2p/circuit-relay-v2";
import { toList } from "../gleam.mjs";

let _libp2p = null;

// ── Timers ──────────────────────────────────────────────────────────

// setTimeout wrapper for Gleam FFI.
export function set_timeout(callback, ms) {
  setTimeout(callback, ms);
}

// ── libp2p lifecycle ────────────────────────────────────────────────

// Create and start a libp2p node.
// Callbacks:
//   on_ready(peer_id_str)            — node is online
//   on_peer_connect(peer_id_str)     — a peer connected
//   on_peer_disconnect(peer_id_str)  — a peer disconnected
export function init_libp2p(on_ready, on_peer_connect, on_peer_disconnect) {
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
        on_peer_connect(event.detail.toString());
      });
      libp2p.addEventListener("peer:disconnect", (event) => {
        on_peer_disconnect(event.detail.toString());
      });

      on_ready(libp2p.peerId.toString());
    })
    .catch((err) => {
      console.error("Failed to create libp2p node:", err);
    });
}

// Get the local peer ID as a string.
export function get_local_peer_id() {
  if (!_libp2p) return "";
  return _libp2p.peerId.toString();
}

// ── Dialling ────────────────────────────────────────────────────────

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

// ── Queries ─────────────────────────────────────────────────────────

// Get this node's multiaddrs as a Gleam List of strings.
export function get_multiaddrs() {
  if (!_libp2p) return toList([]);
  return toList(_libp2p.getMultiaddrs().map((ma) => ma.toString()));
}

// Get connected peer IDs as a Gleam List of strings.
export function get_connected_peers() {
  if (!_libp2p) return toList([]);
  return toList(_libp2p.getPeers().map((p) => p.toString()));
}

// Get all connections as a Gleam List of [peer_id, remote_addr_string].
// No filtering or transport classification — Gleam handles that.
export function get_all_connections() {
  if (!_libp2p) return toList([]);
  const conns = _libp2p.getConnections();
  return toList(
    conns.map((conn) =>
      toList([conn.remotePeer.toString(), conn.remoteAddr.toString()])
    )
  );
}

// ── Protocol messaging ──────────────────────────────────────────────

// Read a full message from a libp2p stream (concatenate all chunks).
async function readStream(stream) {
  const chunks = [];
  try {
    for await (const chunk of stream) {
      if (chunk == null || typeof chunk.subarray !== "function") continue;
      chunks.push(chunk.subarray());
    }
  } catch (err) {
    if (chunks.length === 0) throw err;
  }
  const bytes = new Uint8Array(
    chunks.reduce((acc, c) => acc + c.length, 0)
  );
  let offset = 0;
  for (const c of chunks) {
    bytes.set(c, offset);
    offset += c.length;
  }
  return new TextDecoder().decode(bytes);
}

// Read raw JSON from a stream (used by discovery with rust-libp2p codec).
async function readRawJson(stream) {
  const text = await readStream(stream);
  return JSON.parse(text);
}

// Register a handler for a libp2p protocol.
// on_message(sender_peer_id_str, message_text) is called for each incoming message.
export function register_protocol_handler(protocol, on_message) {
  if (!_libp2p) return;
  _libp2p.handle(
    protocol,
    async (stream, connection) => {
      try {
        const text = await readStream(stream);
        const sender = connection.remotePeer.toString();
        on_message(sender, text);
      } catch (err) {
        console.debug(`Protocol handler error (${protocol}):`, err.message);
      }
    },
    { runOnLimitedConnection: true }
  );
}

// Send a message to a specific peer via a protocol stream.
// Fire-and-forget: calls on_ok() on success, on_error(msg) on failure.
export function send_protocol_message(peer_id_str, protocol, message_text, on_ok, on_error) {
  if (!_libp2p) {
    on_error("libp2p not initialised");
    return;
  }
  const peerId = _findPeerId(peer_id_str);
  if (!peerId) {
    on_error("peer not found");
    return;
  }
  const encoded = new TextEncoder().encode(message_text);
  _libp2p
    .dialProtocol(peerId, protocol, { runOnLimitedConnection: true })
    .then((stream) => {
      stream.send(encoded);
      return stream.close();
    })
    .then(() => on_ok())
    .catch((err) => on_error(err.toString()));
}

// Send a message to a peer, ignoring errors (fire-and-forget).
export function send_protocol_message_fire(peer_id_str, protocol, message_text) {
  if (!_libp2p) return;
  const peerId = _findPeerId(peer_id_str);
  if (!peerId) return;
  const encoded = new TextEncoder().encode(message_text);
  _libp2p
    .dialProtocol(peerId, protocol, { runOnLimitedConnection: true })
    .then((stream) => {
      stream.send(encoded);
      return stream.close();
    })
    .catch(() => {});
}

// Look up a PeerId object by its string representation.
function _findPeerId(peerIdStr) {
  if (!_libp2p) return null;
  for (const pid of _libp2p.getPeers()) {
    if (pid.toString() === peerIdStr) return pid;
  }
  return null;
}

// ── Audio: microphone ───────────────────────────────────────────────

let _localStream = null; // MediaStream from getUserMedia

// Acquire the microphone. Does NOT create any peer connections.
// Calls on_ok() on success, on_error(msg) on failure.
// If already acquired, just re-enables tracks (unmute).
export function acquire_microphone(on_ok, on_error) {
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
      on_ok();
    })
    .catch((err) => {
      on_error(err.toString());
    });
}

// Mute mic: disable local audio tracks but keep stream alive.
export function mute_microphone() {
  if (_localStream) {
    for (const track of _localStream.getAudioTracks()) {
      track.enabled = false;
    }
  }
}

// Release the microphone entirely (stop tracks, free device).
export function release_microphone() {
  if (_localStream) {
    for (const track of _localStream.getTracks()) {
      track.stop();
    }
    _localStream = null;
  }
}

// Returns true if mic is acquired and enabled.
export function is_microphone_active() {
  if (!_localStream) return false;
  const tracks = _localStream.getAudioTracks();
  return tracks.length > 0 && tracks[0].enabled;
}

// Returns true if mic is acquired (even if muted).
export function has_microphone() {
  return _localStream !== null;
}

// ── Audio: remote playback ──────────────────────────────────────────

const _remoteAudios = new Map(); // peer ID string -> HTMLAudioElement

// Unmute all remote audio elements so the user can hear incoming streams.
export function unmute_remote_audio() {
  for (const audio of _remoteAudios.values()) {
    audio.muted = false;
    audio.autoplay = true;
    if (audio.srcObject) {
      audio.play().catch(() => {});
    }
  }
}

// Mute and pause all remote audio elements.
export function mute_remote_audio() {
  for (const audio of _remoteAudios.values()) {
    audio.muted = true;
    audio.pause();
  }
}

// Returns true if we are receiving live audio from any peer.
export function is_receiving_audio() {
  for (const audio of _remoteAudios.values()) {
    if (!audio.srcObject) continue;
    const tracks = audio.srcObject.getAudioTracks();
    if (tracks.some((t) => t.readyState === "live")) return true;
  }
  return false;
}

// Get or create a hidden <audio> element for a remote peer.
function ensureRemoteAudioFor(peerId, muted) {
  if (_remoteAudios.has(peerId)) return _remoteAudios.get(peerId);
  const audio = document.createElement("audio");
  audio.autoplay = true;
  audio.muted = muted;
  audio.style.display = "none";
  document.body.appendChild(audio);
  _remoteAudios.set(peerId, audio);
  return audio;
}

// Remove the audio element for a peer.
function removeRemoteAudioFor(peerId) {
  const audio = _remoteAudios.get(peerId);
  if (audio) {
    audio.pause();
    audio.srcObject = null;
    audio.remove();
    _remoteAudios.delete(peerId);
  }
}

// ── Audio: WebRTC peer connections ──────────────────────────────────
//
// Audio uses standalone RTCPeerConnection objects, separate from
// libp2p's internal WebRTC transport. libp2p streams are used only
// for signaling (SDP offer/answer + ICE candidate exchange).
//
// Gleam drives the lifecycle: which peers to connect to, when to
// reconnect, etc. JS just manages the RTCPeerConnection objects
// and the signaling protocol handler.

const AUDIO_SIGNALING_PROTOCOL = "/sunset/audio-signaling/1.0.0";

const STUN_SERVERS = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

const _audioPCs = new Map(); // peer ID string -> RTCPeerConnection

// Send an audio signaling message to a peer via libp2p stream.
async function sendAudioSignaling(remotePeerId, message) {
  const encoded = new TextEncoder().encode(JSON.stringify(message));
  try {
    const stream = await _libp2p.dialProtocol(
      remotePeerId,
      AUDIO_SIGNALING_PROTOCOL,
      { runOnLimitedConnection: true }
    );
    await stream.send(encoded);
    await stream.close();
  } catch (err) {
    console.warn(
      `[AudioSignaling] Send failed to ${remotePeerId.toString().slice(-8)}:`,
      err.message
    );
  }
}

// Create a standalone RTCPeerConnection for audio with a remote peer.
// on_state_change(peer_id_str, state) is called whenever the connection
// state changes, so Gleam can react (schedule reconnects, etc).
// audio_muted: whether remote audio elements should start muted.
export function create_audio_pc(peer_id_str, should_offer, audio_muted, on_state_change) {
  // Close existing PC for this peer if any
  const oldPC = _audioPCs.get(peer_id_str);
  if (oldPC) {
    oldPC.close();
    _audioPCs.delete(peer_id_str);
    removeRemoteAudioFor(peer_id_str);
  }

  const remotePeerId = _findPeerId(peer_id_str);
  if (!remotePeerId) {
    console.warn(`[Audio] Cannot create PC: peer ${peer_id_str.slice(-8)} not found`);
    return;
  }

  const pc = new RTCPeerConnection({ iceServers: STUN_SERVERS });
  _audioPCs.set(peer_id_str, pc);

  // ICE candidate trickling
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
    console.log(`[Audio ${peer_id_str.slice(-8)}] Remote track: kind=${event.track.kind}`);
    const audio = ensureRemoteAudioFor(peer_id_str, audio_muted);
    if (event.streams && event.streams.length > 0) {
      audio.srcObject = event.streams[0];
    } else {
      audio.srcObject = new MediaStream([event.track]);
    }
    if (!audio_muted) {
      audio.muted = false;
      audio.play().catch(() => {});
    }
  });

  // Connection state changes — forward to Gleam
  pc.addEventListener("connectionstatechange", () => {
    console.log(`[Audio ${peer_id_str.slice(-8)}] Connection: ${pc.connectionState}`);
    on_state_change(peer_id_str, pc.connectionState);
    // Clean up on terminal states
    if (pc.connectionState === "failed" || pc.connectionState === "closed") {
      removeRemoteAudioFor(peer_id_str);
      _audioPCs.delete(peer_id_str);
    }
  });

  pc.addEventListener("iceconnectionstatechange", () => {
    console.log(`[Audio ${peer_id_str.slice(-8)}] ICE: ${pc.iceConnectionState}`);
  });

  // Add local audio track if we have one
  if (_localStream) {
    const track = _localStream.getAudioTracks()[0];
    if (track) {
      pc.addTrack(track, _localStream);
    }
  }

  // If we're the offerer, create and send the SDP offer
  if (should_offer) {
    (async () => {
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        await sendAudioSignaling(remotePeerId, {
          type: "offer",
          sdp: pc.localDescription.sdp,
        });
        console.log(`[Audio ${peer_id_str.slice(-8)}] Sent offer`);
      } catch (err) {
        console.error(`[Audio ${peer_id_str.slice(-8)}] Failed to create offer:`, err);
      }
    })();
  }
}

// Close an audio PC for a specific peer.
export function close_audio_pc(peer_id_str) {
  const pc = _audioPCs.get(peer_id_str);
  if (pc) {
    pc.close();
    _audioPCs.delete(peer_id_str);
    removeRemoteAudioFor(peer_id_str);
  }
}

// Send a "bye" signaling message to a peer (clean hangup).
export function send_audio_bye(peer_id_str) {
  const remotePeerId = _findPeerId(peer_id_str);
  if (remotePeerId) {
    sendAudioSignaling(remotePeerId, { type: "bye" });
  }
}

// Close all audio PCs and release mic. Gleam should send byes first.
export function close_all_audio_pcs() {
  for (const [peerIdStr, pc] of _audioPCs) {
    pc.close();
    removeRemoteAudioFor(peerIdStr);
  }
  _audioPCs.clear();
  release_microphone();
}

// Register the audio signaling protocol handler.
// Handles: offer, answer, candidate (ICE trickle), bye.
// audio_muted: whether new audio elements should start muted.
// on_state_change: forwarded to created PCs.
// on_bye(peer_id_str): called when a bye is received, so Gleam can react.
export function register_audio_signaling_handler(audio_muted, on_state_change, on_bye) {
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

          // Glare resolution: higher peer ID wins
          if (pc && pc.signalingState === "have-local-offer") {
            const localId = _libp2p.peerId.toString();
            if (localId > peerIdStr) {
              console.log(`[AudioSignaling] Glare: ignoring offer from ${peerIdStr.slice(-8)} (we have priority)`);
              return;
            }
            console.log(`[AudioSignaling] Glare: accepting offer from ${peerIdStr.slice(-8)} (they have priority)`);
            pc.close();
            _audioPCs.delete(peerIdStr);
            pc = null;
          }

          if (!pc) {
            // Create a new PC as answerer
            const newPC = new RTCPeerConnection({ iceServers: STUN_SERVERS });
            _audioPCs.set(peerIdStr, newPC);
            pc = newPC;

            // ICE candidate trickling
            pc.addEventListener("icecandidate", (event) => {
              if (event.candidate) {
                sendAudioSignaling(remotePeerId, {
                  type: "candidate",
                  candidate: event.candidate.toJSON(),
                });
              }
            });

            // Remote track handler
            pc.addEventListener("track", (event) => {
              console.log(`[Audio ${peerIdStr.slice(-8)}] Remote track: kind=${event.track.kind}`);
              const audio = ensureRemoteAudioFor(peerIdStr, audio_muted);
              if (event.streams && event.streams.length > 0) {
                audio.srcObject = event.streams[0];
              } else {
                audio.srcObject = new MediaStream([event.track]);
              }
              if (!audio_muted) {
                audio.muted = false;
                audio.play().catch(() => {});
              }
            });

            // Connection state changes
            pc.addEventListener("connectionstatechange", () => {
              console.log(`[Audio ${peerIdStr.slice(-8)}] Connection: ${pc.connectionState}`);
              on_state_change(peerIdStr, pc.connectionState);
              if (pc.connectionState === "failed" || pc.connectionState === "closed") {
                removeRemoteAudioFor(peerIdStr);
                _audioPCs.delete(peerIdStr);
              }
            });

            pc.addEventListener("iceconnectionstatechange", () => {
              console.log(`[Audio ${peerIdStr.slice(-8)}] ICE: ${pc.iceConnectionState}`);
            });

            // Add local audio track if we have one
            if (_localStream) {
              const track = _localStream.getAudioTracks()[0];
              if (track) {
                pc.addTrack(track, _localStream);
              }
            }
          }

          await pc.setRemoteDescription({ type: "offer", sdp: message.sdp });
          const answer = await pc.createAnswer();
          await pc.setLocalDescription(answer);
          await sendAudioSignaling(remotePeerId, {
            type: "answer",
            sdp: pc.localDescription.sdp,
          });
          console.log(`[AudioSignaling] Sent answer to ${peerIdStr.slice(-8)}`);
        } else if (message.type === "answer") {
          const pc = _audioPCs.get(peerIdStr);
          if (!pc) {
            console.debug(`[AudioSignaling] Answer but no PC for ${peerIdStr.slice(-8)}`);
            return;
          }
          if (pc.signalingState !== "have-local-offer") {
            console.debug(`[AudioSignaling] Answer but state=${pc.signalingState} for ${peerIdStr.slice(-8)}`);
            return;
          }
          await pc.setRemoteDescription({ type: "answer", sdp: message.sdp });
          console.log(`[AudioSignaling] Applied answer from ${peerIdStr.slice(-8)}`);
        } else if (message.type === "candidate") {
          const pc = _audioPCs.get(peerIdStr);
          if (!pc) {
            console.debug(`[AudioSignaling] ICE candidate but no PC for ${peerIdStr.slice(-8)}`);
            return;
          }
          try {
            await pc.addIceCandidate(new RTCIceCandidate(message.candidate));
          } catch (err) {
            console.debug(`[AudioSignaling] Failed to add ICE candidate:`, err.message);
          }
        } else if (message.type === "bye") {
          console.log(`[AudioSignaling] Bye from ${peerIdStr.slice(-8)}`);
          const pc = _audioPCs.get(peerIdStr);
          if (pc) {
            pc.close();
            _audioPCs.delete(peerIdStr);
            removeRemoteAudioFor(peerIdStr);
          }
          on_bye(peerIdStr);
        } else {
          console.warn(`[AudioSignaling] Unknown message type: ${message.type}`);
        }
      } catch (err) {
        console.error("[AudioSignaling] Handler error:", err);
      }
    },
    { runOnLimitedConnection: true }
  );
}

// Get the audio PC connection states as a Gleam List of [peer_id, state].
export function get_audio_pc_states() {
  const results = [];
  for (const [pid, pc] of _audioPCs) {
    results.push(toList([pid, pc.connectionState]));
  }
  return toList(results);
}

// Check if an audio PC exists for a peer and is in a given state.
export function has_audio_pc(peer_id_str) {
  return _audioPCs.has(peer_id_str);
}

// ── Discovery ───────────────────────────────────────────────────────

const DISCOVERY_PROTOCOL = "/sunset/discovery/1.0.0";

// One-shot discovery poll: send our info to the relay, get back peers.
// on_response(json_string) is called with the raw JSON response.
export function poll_discovery(relay_peer_id_str, room, on_response) {
  if (!_libp2p) return;

  const relayPeerId = _findPeerId(relay_peer_id_str);
  if (!relayPeerId) return;

  const addrs = _libp2p.getMultiaddrs().map((ma) => ma.toString());
  const request = {
    room,
    peer_id: _libp2p.peerId.toString(),
    addrs,
  };

  (async () => {
    try {
      const stream = await _libp2p.dialProtocol(relayPeerId, DISCOVERY_PROTOCOL);
      const json = new TextEncoder().encode(JSON.stringify(request));
      stream.send(json);
      await stream.close();

      const response = await readRawJson(stream);
      on_response(JSON.stringify(response));
    } catch (err) {
      console.debug("Discovery poll failed:", err.message);
    }
  })();
}
