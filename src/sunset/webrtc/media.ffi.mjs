import { Ok, Error } from "../../gleam.mjs";

export function get_user_audio(callback) {
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
    callback(new Error("getUserMedia not available (requires HTTPS)"));
    return;
  }
  navigator.mediaDevices
    .getUserMedia({ audio: true, video: false })
    .then((stream) => {
      const track = stream.getAudioTracks()[0];
      callback(new Ok([track, stream]));
    })
    .catch((err) => callback(new Error(err.toString())));
}

export function stop_track(track) {
  if (track && typeof track.stop === "function") {
    track.stop();
  }
}

export function play_stream(stream) {
  const audio = document.createElement("audio");
  audio.srcObject = stream;
  audio.autoplay = true;
  audio.style.display = "none";
  document.body.appendChild(audio);
  return audio;
}

export function stop_playback(handle) {
  if (!handle) return;
  handle.pause();
  handle.srcObject = null;
  handle.remove();
}
