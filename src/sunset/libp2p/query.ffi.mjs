import { toList } from "../../gleam.mjs";
import { get_node } from "./node.ffi.mjs";

export function get_multiaddrs() {
  const libp2p = get_node();
  if (!libp2p) return toList([]);
  return toList(libp2p.getMultiaddrs().map((ma) => ma.toString()));
}

export function get_connected_peers() {
  const libp2p = get_node();
  if (!libp2p) return toList([]);
  return toList(libp2p.getPeers().map((p) => p.toString()));
}

export function get_all_connections() {
  const libp2p = get_node();
  if (!libp2p) return toList([]);
  const conns = libp2p.getConnections();
  return toList(
    conns.map((conn) =>
      toList([conn.remotePeer.toString(), conn.remoteAddr.toString()])
    )
  );
}
