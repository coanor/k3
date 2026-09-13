use serde_json::Value;

#[test]
fn fast_mdx_example_preserves_the_models_native_segment_size() {
    let config: Value =
        serde_json::from_str(include_str!("../../../docs/library-config.example.json"))
            .expect("library config example must be valid JSON");
    let separation = config["separation"]
        .as_object()
        .expect("library config example must contain separation settings");

    assert_eq!(separation["profile"], "fast");
    assert_eq!(separation["model"], "uvr-mdx-karaoke-2");
    assert!(
        !separation.contains_key("segment_size"),
        "fast MDX example must not override the model-native segment size"
    );
}
