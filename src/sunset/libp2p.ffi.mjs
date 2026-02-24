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

      libp2p.addEventListener("peer:connect", () => {});
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
let _remoteAudio = null;
let _senders = []; // { pc, sender, peerId } references for cleanup
const _pcToPeer = new Map(); // RTCPeerConnection -> PeerId object
const _attachedPCs = new Set(); // PCs we've already attached listeners to

// Get the RTCPeerConnection from a libp2p connection object.
// Uses internal property path: conn.maConn.peerConnection
// (TypeScript `private` compiles to plain JS properties.)
function getPeerConnection(conn) {
  return conn.maConn?.peerConnection ?? null;
}

// Get all WebRTC connections with their peer IDs and RTCPeerConnections.
// Returns array of { peerId, pc, conn }.
function getWebRTCPeers() {
  if (!_libp2p) return [];
  const results = [];
  for (const conn of _libp2p.getConnections()) {
    // Only WebRTC connections have an RTCPeerConnection
    const pc = getPeerConnection(conn);
    if (pc == null) continue;
    const peerId = conn.remotePeer; // PeerId object, not string
    _pcToPeer.set(pc, peerId);
    results.push({ peerId, pc, conn });
  }
  return results;
}

// Ensure we have a hidden <audio> element for remote playback.
function ensureRemoteAudio() {
  if (_remoteAudio) return _remoteAudio;
  _remoteAudio = document.createElement("audio");
  _remoteAudio.autoplay = true;
  _remoteAudio.id = "remote-audio";
  _remoteAudio.style.display = "none";
  document.body.appendChild(_remoteAudio);
  return _remoteAudio;
}

// Read a full message from a libp2p v3 stream.
async function readStream(stream) {
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
  return new TextDecoder().decode(bytes);
}

// Send a signaling message to a peer via the libp2p signaling protocol.
async function sendSignalingMessage(peerId, message) {
  const stream = await _libp2p.dialProtocol(peerId, SIGNALING_PROTOCOL);
  const encoded = new TextEncoder().encode(JSON.stringify(message));
  stream.send(encoded);
  await stream.close();
}

// Attach negotiationneeded + track listeners to a peer connection.
// Safe to call multiple times — will only attach once per PC.
function attachPCHandlers(pc) {
  if (_attachedPCs.has(pc)) return;
  _attachedPCs.add(pc);

  // When addTrack() or removeTrack() changes the SDP, create and send an offer.
  pc.addEventListener("negotiationneeded", async () => {
      const peerId = _pcToPeer.get(pc);
    if (!peerId) {
      console.warn("negotiationneeded fired but no peer ID mapped for PC");
      return;
    }
    try {
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      await sendSignalingMessage(peerId, {
        type: "offer",
        sdp: pc.localDescription.sdp,
      });
      console.log("Sent renegotiation offer to", peerId.toString());
    } catch (err) {
      console.error("Failed to send renegotiation offer:", err);
    }
  });

  // When a remote peer adds a track, play it.
  pc.addEventListener("track", (event) => {
    const audio = ensureRemoteAudio();
    if (event.streams && event.streams.length > 0) {
      audio.srcObject = event.streams[0];
    } else {
      const stream = new MediaStream([event.track]);
      audio.srcObject = stream;
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
      const pc = getPeerConnection(connection);
      if (!pc) {
        console.warn(
          "Signaling message from non-WebRTC peer:",
          remotePeerId.toString(),
        );
        return;
      }
      _pcToPeer.set(pc, remotePeerId);
      attachPCHandlers(pc);

      if (message.type === "offer") {
        await pc.setRemoteDescription({
          type: "offer",
          sdp: message.sdp,
        });
        const answer = await pc.createAnswer();
        await pc.setLocalDescription(answer);
        await sendSignalingMessage(remotePeerId, {
          type: "answer",
          sdp: pc.localDescription.sdp,
        });
      } else if (message.type === "answer") {
        await pc.setRemoteDescription({
          type: "answer",
          sdp: message.sdp,
        });
      } else {
        console.warn("Unknown signaling message type:", message.type);
      }
    } catch (err) {
      console.error("Signaling handler error:", err);
    }
  });
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

// Returns true if we are receiving remote audio.
export function is_receiving_audio() {
  if (!_remoteAudio || !_remoteAudio.srcObject) return false;
  const tracks = _remoteAudio.srcObject.getAudioTracks();
  return tracks.some((t) => t.readyState === "live");
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
