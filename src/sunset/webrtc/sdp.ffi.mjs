import { Ok, Error } from "../../gleam.mjs";

export function create_offer(pc, callback) {
  pc.createOffer()
    .then((offer) => pc.setLocalDescription(offer).then(() => offer))
    .then((offer) => callback(new Ok(JSON.stringify(offer))))
    .catch((err) => callback(new Error(err.toString())));
}

export function create_answer(pc, callback) {
  pc.createAnswer()
    .then((answer) => pc.setLocalDescription(answer).then(() => answer))
    .then((answer) => callback(new Ok(JSON.stringify(answer))))
    .catch((err) => callback(new Error(err.toString())));
}

export function set_remote_description(pc, sdp_json, callback) {
  try {
    const desc = JSON.parse(sdp_json);
    pc.setRemoteDescription(desc)
      .then(() => callback(new Ok(undefined)))
      .catch((err) => callback(new Error(err.toString())));
  } catch (err) {
    callback(new Error(err.toString()));
  }
}

export function add_ice_candidate(pc, candidate_json, callback) {
  try {
    const candidate = JSON.parse(candidate_json);
    pc.addIceCandidate(candidate)
      .then(() => callback(new Ok(undefined)))
      .catch((err) => callback(new Error(err.toString())));
  } catch (err) {
    callback(new Error(err.toString()));
  }
}
