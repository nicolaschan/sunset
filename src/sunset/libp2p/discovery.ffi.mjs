import { toList } from "../../gleam.mjs";
import { get_node, find_peer_id } from "./node.ffi.mjs";

const DISCOVERY_PROTOCOL = "/sunset/discovery/1.0.0";

async function read_stream(stream) {
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

function read_raw_json(stream) {
  return read_stream(stream).then((text) => JSON.parse(text));
}

export function poll_discovery(relay_peer_id_str, room, on_peer) {
  const libp2p = get_node();
  if (!libp2p) return;

  const relayPeerId = find_peer_id(relay_peer_id_str);
  if (!relayPeerId) return;

  const addrs = libp2p.getMultiaddrs().map((ma) => ma.toString());
  const request = {
    room,
    peer_id: libp2p.peerId.toString(),
    addrs,
  };

  (async () => {
    try {
      const stream = await libp2p.dialProtocol(relayPeerId, DISCOVERY_PROTOCOL);
      const json = new TextEncoder().encode(JSON.stringify(request));
      stream.send(json);
      await stream.close();

      const response = await read_raw_json(stream);
      const peers = response?.peers || [];
      for (const peer of peers) {
        if (peer.peer_id && Array.isArray(peer.addrs)) {
          on_peer(peer.peer_id, toList(peer.addrs));
        }
      }
    } catch (err) {
      console.debug("Discovery poll failed:", err.message);
    }
  })();
}
