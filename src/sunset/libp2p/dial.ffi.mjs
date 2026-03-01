import { Ok, Error } from "../../gleam.mjs";
import { multiaddr } from "@multiformats/multiaddr";
import { get_node } from "./node.ffi.mjs";

export function dial_multiaddr(addr_str, callback) {
  const libp2p = get_node();
  if (!libp2p) {
    callback(new Error("libp2p not initialised"));
    return;
  }
  try {
    const maddr = multiaddr(addr_str);
    libp2p
      .dial(maddr)
      .then(() => callback(new Ok(undefined)))
      .catch((err) => callback(new Error(err.toString())));
  } catch (err) {
    callback(new Error(err.toString()));
  }
}
