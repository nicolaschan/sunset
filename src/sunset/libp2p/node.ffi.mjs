import { createLibp2p } from "libp2p";
import { identify } from "@libp2p/identify";
import { noise } from "@chainsafe/libp2p-noise";
import { yamux } from "@chainsafe/libp2p-yamux";
import { webSockets } from "@libp2p/websockets";
import { webTransport } from "@libp2p/webtransport";
import { webRTC } from "@libp2p/webrtc";
import { circuitRelayTransport } from "@libp2p/circuit-relay-v2";

let _libp2p = null;

export function get_node() {
  return _libp2p;
}

export function find_peer_id(peerIdStr) {
  if (!_libp2p) return null;
  for (const pid of _libp2p.getPeers()) {
    if (pid.toString() === peerIdStr) return pid;
  }
  return null;
}

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

export function get_local_peer_id() {
  if (!_libp2p) return "";
  return _libp2p.peerId.toString();
}
