use std::time::Duration;

/// One timestamped lyric line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsLine {
    /// Position of the lyric in the song.
    pub at: Duration,
    /// Text displayed at this position.
    pub text: String,
    order: usize,
}

/// Parsed LRC lyrics ordered by playback position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricsTimeline {
    lines: Vec<LyricsLine>,
}

impl LyricsTimeline {
    /// Parses timestamped lines from LRC text.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let mut lines = Vec::new();
        let mut order = 0;

        for input_line in input.lines() {
            let (timestamps, text) = timestamps_and_text(input_line);
            for at in timestamps {
                lines.push(LyricsLine {
                    at,
                    text: text.to_owned(),
                    order,
                });
                order += 1;
            }
        }

        lines.sort_by_key(|line| (line.at, line.order));
        Self { lines }
    }

    /// Returns every parsed line in playback order.
    #[must_use]
    pub fn lines(&self) -> &[LyricsLine] {
        &self.lines
    }

    /// Returns the lyric active at `position` after applying a signed offset.
    #[must_use]
    pub fn line_at(&self, position: Duration, offset_ms: i64) -> Option<&LyricsLine> {
        self.active_index(position, offset_ms)
            .map(|index| &self.lines[index])
    }

    /// Returns the index of the lyric active at `position` after a signed offset.
    #[must_use]
    pub fn active_index(&self, position: Duration, offset_ms: i64) -> Option<usize> {
        let adjusted_ms =
            i128::try_from(position.as_millis()).unwrap_or(i128::MAX) + i128::from(offset_ms);
        if adjusted_ms < 0 {
            return None;
        }
        let adjusted = Duration::from_millis(u64::try_from(adjusted_ms).unwrap_or(u64::MAX));

        self.lines
            .partition_point(|line| line.at <= adjusted)
            .checked_sub(1)
    }
}

fn timestamps_and_text(line: &str) -> (Vec<Duration>, &str) {
    let mut rest = line;
    let mut timestamps = Vec::new();

    while let Some(after_open) = rest.strip_prefix('[') {
        let Some(close) = after_open.find(']') else {
            break;
        };
        let token = &after_open[..close];
        let Some(timestamp) = parse_timestamp(token) else {
            break;
        };
        timestamps.push(timestamp);
        rest = &after_open[close + 1..];
    }

    (timestamps, rest)
}

fn parse_timestamp(token: &str) -> Option<Duration> {
    let (minutes, seconds) = token.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 {
        return None;
    }

    let millis = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        3 => fraction.parse::<u64>().ok()?,
        _ => return None,
    };
    Some(Duration::from_millis(
        minutes * 60_000 + seconds * 1_000 + millis,
    ))
}
