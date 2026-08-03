use std::time::Duration;

use k3_core::LyricsTimeline;

#[test]
fn timeline_supports_multiple_timestamps_precision_and_offset() {
    let timeline = LyricsTimeline::parse(
        "[ar:artist]\n[00:01.25][00:03.125]Hello\nplain text\n[00:05.00]World",
    );

    assert_eq!(timeline.lines().len(), 3);
    assert_eq!(timeline.line_at(Duration::from_millis(700), 500), None);
    assert_eq!(
        timeline
            .line_at(Duration::from_millis(750), 500)
            .map(|line| line.text.as_str()),
        Some("Hello")
    );
    assert_eq!(
        timeline
            .line_at(Duration::from_millis(4_500), 500)
            .map(|line| line.text.as_str()),
        Some("World")
    );
}

#[test]
fn timeline_returns_nothing_before_the_first_lyric() {
    let timeline = LyricsTimeline::parse("[00:10.000]Later");

    assert_eq!(timeline.line_at(Duration::from_secs(9), 0), None);
}
