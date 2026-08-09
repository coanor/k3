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

#[test]
fn timeline_reports_the_active_line_index_at_boundaries() {
    let timeline = LyricsTimeline::parse("[00:01.000]One\n[00:02.000]Two\n[00:03.000]Three");

    assert_eq!(timeline.active_index(Duration::from_millis(999), 0), None);
    assert_eq!(timeline.active_index(Duration::from_secs(1), 0), Some(0));
    assert_eq!(
        timeline.active_index(Duration::from_millis(2_500), 0),
        Some(1)
    );
}
