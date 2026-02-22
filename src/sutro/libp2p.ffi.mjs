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

const CHAT_PROTOCOL = "/sutro/chat/1.0.0";
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
  });
}

// Broadcast a message to all connected peers.
// Calls on_ok() when all sends are attempted, on_error(msg) on failure.
export function broadcast_message(text, on_ok, on_error) {
  if (!_libp2p) {
    on_error("libp2p not initialised");
    return;
  }
  const peers = _libp2p.getPeers();
  if (peers.length === 0) {
    on_error("No peers connected");
    return;
  }
  const encoded = new TextEncoder().encode(text);
  const sends = peers.map(async (peerId) => {
    try {
      const stream = await _libp2p.dialProtocol(peerId, CHAT_PROTOCOL);
      stream.send(encoded);
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

const SIGNALING_PROTOCOL = "/sutro/signaling/1.0.0";

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
