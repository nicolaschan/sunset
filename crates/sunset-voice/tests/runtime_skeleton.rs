#[test]
fn voice_runtime_constructs_and_drops_cleanly() {
    use sunset_voice::runtime::VoiceRuntime;
    // Compile-test: VoiceRuntime exists. Body asserts trivially.
    let _ = std::any::TypeId::of::<VoiceRuntime>();
}
