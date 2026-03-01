const DEFAULT_ICE_SERVERS = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

function get_ice_servers() {
  try {
    const params = new URLSearchParams(window.location.search);
    if (params.get("ice_servers") === "none") return [];
  } catch {}
  return DEFAULT_ICE_SERVERS;
}

export function create_pc(
  on_ice_candidate,
  on_state_change,
  on_negotiation_needed,
  on_track
) {
  const pc = new RTCPeerConnection({ iceServers: get_ice_servers() });

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

export function wait_for_ice_gathering(pc, timeout_ms, callback) {
  if (pc.iceGatheringState === "complete") {
    callback();
    return;
  }

  let resolved = false;
  const resolve = () => {
    if (resolved) return;
    resolved = true;
    pc.removeEventListener("icegatheringstatechange", handler);
    callback();
  };

  function handler() {
    if (pc.iceGatheringState === "complete") resolve();
  }

  pc.addEventListener("icegatheringstatechange", handler);
  setTimeout(resolve, timeout_ms);
}
