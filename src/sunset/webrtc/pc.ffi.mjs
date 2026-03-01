const STUN_SERVERS = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

export function create_pc(
  on_ice_candidate,
  on_state_change,
  on_negotiation_needed,
  on_track
) {
  const pc = new RTCPeerConnection({ iceServers: STUN_SERVERS });

  pc.addEventListener("icecandidate", (event) => {
    if (event.candidate) {
      on_ice_candidate(JSON.stringify(event.candidate.toJSON()));
    }
  });

  pc.addEventListener("connectionstatechange", () => {
    on_state_change(pc.connectionState);
  });

  pc.addEventListener("negotiationneeded", () => {
    on_negotiation_needed();
  });

  pc.addEventListener("track", (event) => {
    const stream = event.streams[0] || new MediaStream([event.track]);
    on_track(event.track, stream);
  });

  on_state_change(pc.connectionState);

  return pc;
}

export function close_pc(pc) {
  pc.close();
}

export function wait_for_ice_gathering(pc, callback) {
  if (pc.iceGatheringState === "complete") {
    callback();
    return;
  }
  pc.addEventListener("icegatheringstatechange", function handler() {
    if (pc.iceGatheringState === "complete") {
      pc.removeEventListener("icegatheringstatechange", handler);
      callback();
    }
  });
}
