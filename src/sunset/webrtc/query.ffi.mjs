export function get_connection_state(pc) {
  return pc.connectionState;
}

export function get_local_description(pc) {
  if (!pc.localDescription) return "";
  return JSON.stringify(pc.localDescription);
}

export function get_local_description_type(pc) {
  if (!pc.localDescription) return "";
  return pc.localDescription.type;
}

export function get_signaling_state(pc) {
  return pc.signalingState;
}
