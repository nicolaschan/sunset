const handlers = new Map();

export function set_handler(peer_id, handler) {
  console.warn(`[webrtc:dispatch] set_handler peer=${peer_id.slice(peer_id.length - 6)}`);
  handlers.set(peer_id, handler);
}

export function remove_handler(peer_id) {
  console.warn(`[webrtc:dispatch] remove_handler peer=${peer_id.slice(peer_id.length - 6)}`);
  handlers.delete(peer_id);
}

export function dispatch(peer_id, msg) {
  const handler = handlers.get(peer_id);
  const short = peer_id.slice(peer_id.length - 6);
  const msgPreview = msg.length > 40 ? msg.slice(0, 40) + "..." : msg;
  if (handler) {
    console.warn(`[webrtc:dispatch] dispatch peer=${short} msg=${msgPreview}`);
    handler(msg);
  } else {
    console.warn(`[webrtc:dispatch] NO HANDLER for peer=${short} msg=${msgPreview} (handlers: ${[...handlers.keys()].map(k => k.slice(k.length - 6)).join(",")})`);
  }
}
