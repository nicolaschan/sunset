import { Ok, Error } from "../../gleam.mjs";
import { get_node, find_peer_id } from "./node.ffi.mjs";

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

export function register_protocol_handler(protocol, on_message) {
  const libp2p = get_node();
  if (!libp2p) return;
  libp2p.handle(
    protocol,
    async (stream, connection) => {
      try {
        const text = await read_stream(stream);
        const sender = connection.remotePeer.toString();
        on_message(sender, text);
      } catch (err) {
        console.debug(`Protocol handler error (${protocol}):`, err.message);
      }
    },
    { runOnLimitedConnection: true }
  );
}

export function send_protocol_message(peer_id_str, protocol, message_text, callback) {
  const libp2p = get_node();
  if (!libp2p) {
    callback(new Error("libp2p not initialised"));
    return;
  }
  const peerId = find_peer_id(peer_id_str);
  if (!peerId) {
    callback(new Error("peer not found"));
    return;
  }
  const encoded = new TextEncoder().encode(message_text);
  libp2p
    .dialProtocol(peerId, protocol, { runOnLimitedConnection: true })
    .then((stream) => {
      stream.send(encoded);
      return stream.close();
    })
    .then(() => callback(new Ok(undefined)))
    .catch((err) => callback(new Error(err.toString())));
}

export function send_protocol_message_fire(peer_id_str, protocol, message_text) {
  const libp2p = get_node();
  if (!libp2p) return;
  const peerId = find_peer_id(peer_id_str);
  if (!peerId) return;
  const encoded = new TextEncoder().encode(message_text);
  libp2p
    .dialProtocol(peerId, protocol, { runOnLimitedConnection: true })
    .then((stream) => {
      stream.send(encoded);
      return stream.close();
    })
    .catch(() => { });
}
