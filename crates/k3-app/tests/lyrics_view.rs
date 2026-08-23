use std::time::Duration;

use k3_app::lyric_window;
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
