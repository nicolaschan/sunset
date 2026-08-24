import gleeunit/should
import sunset_web
import sunset_web/domain.{type Channel, Channel, ChannelId, TextChannel, Voice}

/// The channel list a fresh room starts with: the default text channel
/// plus the placeholder voice channel. Note the voice channel's id is
/// "voice" while its name is "general" — ids are not namespaced by
/// kind, so a text channel can share an id with a voice one.
fn initial() -> List(Channel) {
  [
    Channel(id: ChannelId("general"), name: "general", kind: TextChannel),
    Channel(id: ChannelId("voice"), name: "general", kind: Voice),
  ]
}

pub fn observed_labels_become_text_channels_test() {
  sunset_web.merge_observed_channels(initial(), ["links", "general"])
  |> should.equal([
    Channel(id: ChannelId("general"), name: "general", kind: TextChannel),
    Channel(id: ChannelId("links"), name: "links", kind: TextChannel),
    Channel(id: ChannelId("voice"), name: "general", kind: Voice),
  ])
}

pub fn default_channel_is_added_when_unobserved_test() {
  sunset_web.merge_observed_channels(initial(), ["links"])
  |> should.equal([
    Channel(id: ChannelId("general"), name: "general", kind: TextChannel),
    Channel(id: ChannelId("links"), name: "links", kind: TextChannel),
    Channel(id: ChannelId("voice"), name: "general", kind: Voice),
  ])
}

pub fn voice_channels_survive_an_empty_snapshot_test() {
  sunset_web.merge_observed_channels(initial(), [])
  |> should.equal([
    Channel(id: ChannelId("general"), name: "general", kind: TextChannel),
    Channel(id: ChannelId("voice"), name: "general", kind: Voice),
  ])
}

/// A user can create a text channel literally named "voice" — the
/// new-channel input accepts it, and once they post there the engine
/// echoes "voice" back in the observed snapshot. Its id then collides
/// with the placeholder voice channel's id. The rail must render it as
/// its own text row and still emit exactly one voice block.
pub fn observed_label_colliding_with_a_voice_id_test() {
  sunset_web.merge_observed_channels(initial(), ["general", "voice"])
  |> should.equal([
    Channel(id: ChannelId("general"), name: "general", kind: TextChannel),
    Channel(id: ChannelId("voice"), name: "voice", kind: TextChannel),
    Channel(id: ChannelId("voice"), name: "general", kind: Voice),
  ])
}
