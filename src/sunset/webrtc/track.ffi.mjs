export function add_track(pc, track, stream) {
  return pc.addTrack(track, stream);
}

export function remove_track(pc, sender) {
  try {
    pc.removeTrack(sender);
  } catch (_) {
    // already removed or pc closed
  }
}
