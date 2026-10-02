use std::time::Duration;

use k3_app::{LyricCountdown, lyric_countdown, lyric_window};
use k3_core::LyricsTimeline;

#[test]
fn lyric_window_keeps_more_upcoming_lines_visible() {
    let timeline = LyricsTimeline::parse(
        "[00:00.000]0\n[00:01.000]1\n[00:02.000]2\n[00:03.000]3\n[00:04.000]4\n[00:05.000]5\n[00:06.000]6\n[00:07.000]7",
    );

    let window = lyric_window(&timeline, Duration::from_millis(3_500), 5);

    assert_eq!(window.range, 2..7);
    assert_eq!(window.current, Some(3));
}

#[test]
fn lyric_countdown_cues_only_the_final_three_seconds() {
    let timeline = LyricsTimeline::parse("[00:02.000]first\n[00:07.000]second");

    assert_eq!(
        lyric_countdown(&timeline, Duration::ZERO),
        Some(LyricCountdown {
            index: 0,
            seconds: 2,
        })
    );
    assert_eq!(
        lyric_countdown(&timeline, Duration::from_millis(4_001)),
        Some(LyricCountdown {
            index: 1,
            seconds: 3,
        })
    );
    assert_eq!(
        lyric_countdown(&timeline, Duration::from_millis(6_001)),
        Some(LyricCountdown {
            index: 1,
            seconds: 1,
        })
    );
    assert_eq!(lyric_countdown(&timeline, Duration::from_secs(7)), None);
}

#[test]
fn lyric_countdown_ignores_rapid_lyrics() {
    let timeline = LyricsTimeline::parse("[00:02.000]fast\n[00:02.900]lyrics");

    assert_eq!(
        lyric_countdown(&timeline, Duration::from_millis(2_100)),
        None
    );
}
