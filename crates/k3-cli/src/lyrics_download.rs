use std::{
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};

use k3_core::{LyricsTimeline, Project, ProjectPath};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::Accessor,
};
use serde::Deserialize;

const LRCLIB_SEARCH_URL: &str = "https://lrclib.net/api/search";
const NETEASE_SEARCH_URL: &str = "https://music.163.com/api/search/get/web";
const NETEASE_LYRIC_URL: &str = "https://music.163.com/api/song/lyric";
const MAX_DURATION_DIFFERENCE_SECONDS: f64 = 8.0;

#[derive(Debug, PartialEq, Eq)]
pub enum LyricsDownload {
    AlreadyPresent,
    Downloaded { track: String, artist: String },
    NotFound,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LyricsProgress {
    CheckingLocal,
    SearchingOnline {
        source: &'static str,
        title: String,
        artist: Option<String>,
    },
    FallingBackToTitle {
        source: &'static str,
        title: String,
    },
    RetryingOnline {
        source: &'static str,
        reason: String,
    },
    FoundOnline {
        source: &'static str,
        track: String,
        artist: String,
        duration_seconds: u64,
    },
    Saving {
        relative_path: String,
    },
}

impl fmt::Display for LyricsProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CheckingLocal => formatter.write_str("Checking local lyrics..."),
            Self::SearchingOnline {
                source,
                title,
                artist,
            } => {
                if let Some(artist) = artist {
                    write!(formatter, "Searching {source}: {artist} - {title}...")
                } else {
                    write!(formatter, "Searching {source}: {title}...")
                }
            }
            Self::FallingBackToTitle { source, title } => {
                write!(
                    formatter,
                    "No {source} artist match; retrying by title only: {title}..."
                )
            }
            Self::RetryingOnline { source, reason } => {
                write!(
                    formatter,
                    "{source} request failed: {reason}; retrying (2/2)..."
                )
            }
            Self::FoundOnline {
                source,
                track,
                artist,
                duration_seconds,
            } => write!(
                formatter,
                "Found synced lyrics on {source}: {artist} - {track} ({duration_seconds}s)"
            ),
            Self::Saving { relative_path } => {
                write!(formatter, "Saving lyrics: {relative_path}")
            }
        }
    }
}

/// Downloads synchronized lyrics when the project has none and attaches the local LRC file.
///
/// Network lookup, metadata extraction, candidate matching and atomic persistence are hidden
/// behind this interface so recording only needs to handle the observable outcome.
///
/// # Errors
///
/// Returns an error when all configured sources fail or the selected lyrics cannot be persisted.
pub fn download_missing_lyrics(
    project: &mut Project,
    netease_fallback: bool,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    let lrclib = LrclibCatalog::new();
    if netease_fallback {
        let netease = NeteaseCatalog::new();
        download_with_catalogs(project, &[&lrclib, &netease], progress)
    } else {
        download_with_catalogs(project, &[&lrclib], progress)
    }
}

#[cfg(test)]
fn download_with_catalog(
    project: &mut Project,
    catalog: &dyn LyricsCatalog,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    download_with_catalogs(project, &[catalog], progress)
}

fn download_with_catalogs(
    project: &mut Project,
    catalogs: &[&dyn LyricsCatalog],
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    progress(&LyricsProgress::CheckingLocal);
    if let Some(relative_path) = project.lyrics() {
        let configured_path = project.root().join(Path::new(relative_path.as_str()));
        if configured_path.is_file() {
            return Ok(LyricsDownload::AlreadyPresent);
        }
    }

    let relative_path = downloaded_lyrics_path(project)?;
    let destination = project.root().join(Path::new(relative_path.as_str()));
    if destination.is_file() {
        let contents = fs::read_to_string(&destination)?;
        if !LyricsTimeline::parse(&contents).lines().is_empty() {
            project.set_lyrics(relative_path)?;
            return Ok(LyricsDownload::AlreadyPresent);
        }
        return Err(format!(
            "Local lyrics contain no recognizable LRC timeline; file not overwritten: {}",
            destination.display()
        )
        .into());
    }

    let lookup = lookup_from_audio(project);
    let mut selected = None;
    let mut successful_search = false;
    let mut failures = Vec::new();
    for catalog in catalogs {
        progress(&LyricsProgress::SearchingOnline {
            source: catalog.name(),
            title: lookup.title.clone(),
            artist: lookup.artist.clone(),
        });
        match search_candidates(*catalog, &lookup, progress) {
            Ok(candidates) => {
                successful_search = true;
                selected = select_candidate(candidates, lookup.duration)
                    .map(|candidate| (catalog.name(), candidate));
                if selected.is_some() {
                    break;
                }
            }
            Err(error) => failures.push(format!("{}: {error}", catalog.name())),
        }
    }
    let Some((source, candidate)) = selected else {
        if !successful_search && !failures.is_empty() {
            return Err(format!("All lyric sources failed: {}", failures.join("; ")).into());
        }
        return Ok(LyricsDownload::NotFound);
    };
    if LyricsTimeline::parse(&candidate.synced_lyrics)
        .lines()
        .is_empty()
    {
        return Ok(LyricsDownload::NotFound);
    }

    progress(&LyricsProgress::FoundOnline {
        source,
        track: candidate.track_name.clone(),
        artist: candidate.artist_name.clone(),
        duration_seconds: Duration::try_from_secs_f64(candidate.duration.max(0.0))
            .map_or(0, |duration| duration.as_secs()),
    });
    progress(&LyricsProgress::Saving {
        relative_path: relative_path.as_str().to_owned(),
    });
    write_atomically(&destination, &candidate.synced_lyrics)?;
    project.set_lyrics(relative_path)?;
    Ok(LyricsDownload::Downloaded {
        track: candidate.track_name,
        artist: candidate.artist_name,
    })
}

fn search_candidates(
    catalog: &dyn LyricsCatalog,
    lookup: &LyricsLookup,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
    let candidates = search_with_retry(catalog, lookup, progress)?;
    if !candidates.is_empty() || lookup.artist.is_none() {
        return Ok(candidates);
    }

    progress(&LyricsProgress::FallingBackToTitle {
        source: catalog.name(),
        title: lookup.title.clone(),
    });
    let title_only = LyricsLookup {
        title: lookup.title.clone(),
        artist: None,
        duration: lookup.duration,
    };
    search_with_retry(catalog, &title_only, progress)
}

#[derive(Debug)]
struct LyricsLookup {
    title: String,
    artist: Option<String>,
    duration: Option<Duration>,
}

fn lookup_from_audio(project: &Project) -> LyricsLookup {
    let tagged = lofty::read_from_path(project.source_path()).ok();
    let tag = tagged
        .as_ref()
        .and_then(|file| file.primary_tag().or_else(|| file.first_tag()));
    let title = tag
        .and_then(Accessor::title)
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| project.title().to_owned(), std::borrow::Cow::into_owned);
    let artist = tag
        .and_then(Accessor::artist)
        .filter(|value| !value.trim().is_empty())
        .map(std::borrow::Cow::into_owned);
    let duration = tagged
        .as_ref()
        .map(AudioFile::properties)
        .map(lofty::properties::FileProperties::duration)
        .filter(|duration| !duration.is_zero());
    LyricsLookup {
        title,
        artist,
        duration,
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LyricsCandidate {
    track_name: String,
    artist_name: String,
    duration: f64,
    synced_lyrics: Option<String>,
}

trait LyricsCatalog {
    fn name(&self) -> &'static str {
        "test catalog"
    }

    fn search(&self, lookup: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>>;
}

struct LrclibCatalog {
    agent: ureq::Agent,
}

impl LrclibCatalog {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(format!("k3/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

fn search_with_retry(
    catalog: &dyn LyricsCatalog,
    lookup: &LyricsLookup,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
    match catalog.search(lookup) {
        Ok(candidates) => Ok(candidates),
        Err(first_error) => {
            progress(&LyricsProgress::RetryingOnline {
                source: catalog.name(),
                reason: first_error.to_string(),
            });
            catalog.search(lookup).map_err(|retry_error| {
                format!(
                    "Both {} requests failed; first: {first_error}; retry: {retry_error}",
                    catalog.name()
                )
                .into()
            })
        }
    }
}

impl LyricsCatalog for LrclibCatalog {
    fn name(&self) -> &'static str {
        "LRCLIB"
    }

    fn search(&self, lookup: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
        let request = self.agent.get(LRCLIB_SEARCH_URL);
        let mut response = if let Some(artist) = &lookup.artist {
            request
                .query("track_name", &lookup.title)
                .query("artist_name", artist)
                .call()?
        } else {
            request.query("q", &lookup.title).call()?
        };
        Ok(response.body_mut().read_json()?)
    }
}

struct NeteaseCatalog {
    agent: ureq::Agent,
}

impl NeteaseCatalog {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(format!("k3/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

#[derive(Deserialize)]
struct NeteaseSearchResponse {
    result: Option<NeteaseSearchResult>,
}

#[derive(Deserialize)]
struct NeteaseSearchResult {
    #[serde(default)]
    songs: Vec<NeteaseSong>,
}

#[derive(Deserialize)]
struct NeteaseSong {
    id: u64,
    name: String,
    duration: f64,
    #[serde(default)]
    artists: Vec<NeteaseArtist>,
}

#[derive(Deserialize)]
struct NeteaseArtist {
    name: String,
}

#[derive(Deserialize)]
struct NeteaseLyricResponse {
    lrc: Option<NeteaseLyrics>,
}

#[derive(Deserialize)]
struct NeteaseLyrics {
    lyric: String,
}

impl LyricsCatalog for NeteaseCatalog {
    fn name(&self) -> &'static str {
        "NetEase Cloud Music"
    }

    fn search(&self, lookup: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
        let query = lookup.artist.as_ref().map_or_else(
            || lookup.title.clone(),
            |artist| format!("{} {artist}", lookup.title),
        );
        let mut response = self
            .agent
            .get(NETEASE_SEARCH_URL)
            .query("s", &query)
            .query("type", "1")
            .query("offset", "0")
            .query("total", "true")
            .query("limit", "20")
            .call()?;
        let search: NeteaseSearchResponse = response.body_mut().read_json()?;
        let songs = search.result.map_or_else(Vec::new, |result| result.songs);
        let expected_title = normalize_match_text(&lookup.title);
        let expected_artist = lookup.artist.as_deref().map(normalize_match_text);
        let mut candidates = Vec::new();
        for song in songs.into_iter().filter(|song| {
            netease_song_matches(
                song,
                &expected_title,
                expected_artist.as_deref(),
                lookup.duration,
            )
        }) {
            let Ok(mut lyric_response) = self
                .agent
                .get(NETEASE_LYRIC_URL)
                .query("id", song.id.to_string())
                .query("lv", "-1")
                .query("kv", "-1")
                .query("tv", "-1")
                .call()
            else {
                continue;
            };
            let lyrics: NeteaseLyricResponse = lyric_response.body_mut().read_json()?;
            let artist_name = song
                .artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            candidates.push(LyricsCandidate {
                track_name: song.name,
                artist_name,
                duration: song.duration / 1_000.0,
                synced_lyrics: lyrics.lrc.map(|lyrics| lyrics.lyric),
            });
        }
        Ok(candidates)
    }
}

fn netease_song_matches(
    song: &NeteaseSong,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
) -> bool {
    let candidate_title = normalize_match_text(&song.name);
    let text_matches = expected_artist.map_or_else(
        || {
            candidate_title == expected_title
                || (!candidate_title.is_empty()
                    && expected_title.contains(&candidate_title)
                    && song.artists.iter().any(|artist| {
                        let candidate_artist = normalize_match_text(&artist.name);
                        !candidate_artist.is_empty() && expected_title.contains(&candidate_artist)
                    }))
        },
        |expected| {
            candidate_title == expected_title
                && song
                    .artists
                    .iter()
                    .any(|artist| normalize_match_text(&artist.name) == expected)
        },
    );
    text_matches
        && expected_duration.is_none_or(|duration| {
            (song.duration / 1_000.0 - duration.as_secs_f64()).abs()
                <= MAX_DURATION_DIFFERENCE_SECONDS
        })
}

fn normalize_match_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn select_candidate(
    candidates: Vec<LyricsCandidate>,
    expected_duration: Option<Duration>,
) -> Option<SelectedLyrics> {
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let duration_difference = expected_duration.map_or(0.0, |expected| {
                (candidate.duration - expected.as_secs_f64()).abs()
            });
            let synced_lyrics = candidate
                .synced_lyrics
                .filter(|lyrics| !lyrics.trim().is_empty())?;
            (expected_duration.is_none() || duration_difference <= MAX_DURATION_DIFFERENCE_SECONDS)
                .then_some((
                    duration_difference,
                    SelectedLyrics {
                        track_name: candidate.track_name,
                        artist_name: candidate.artist_name,
                        duration: candidate.duration,
                        synced_lyrics,
                    },
                ))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, selected)| selected)
}

struct SelectedLyrics {
    track_name: String,
    artist_name: String,
    duration: f64,
    synced_lyrics: String,
}

fn downloaded_lyrics_path(project: &Project) -> Result<ProjectPath, Box<dyn Error>> {
    let source_path = project.source_path();
    let stem = source_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("downloaded");
    Ok(ProjectPath::new(format!("lyrics/{stem}.lrc"))?)
}

fn write_atomically(destination: &Path, lyrics: &str) -> Result<(), Box<dyn Error>> {
    let filename = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("downloaded lyrics path is not valid UTF-8")?;
    let temporary =
        destination.with_file_name(format!(".{filename}.{}.partial", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        file.write_all(lyrics.as_bytes())?;
        if !lyrics.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        fs::rename(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        LyricsCandidate, LyricsCatalog, LyricsDownload, LyricsLookup, NeteaseArtist, NeteaseSong,
        download_with_catalog, download_with_catalogs, netease_song_matches, normalize_match_text,
        search_candidates,
    };
    use k3_core::{CreateProject, FileProjectRepository, ProjectRepository};
    use std::{cell::Cell, error::Error, fs, io, time::Duration};

    struct FakeCatalog {
        candidates: Vec<LyricsCandidate>,
    }

    struct ArtistFallbackCatalog {
        calls: Cell<usize>,
    }

    #[test]
    fn metadata_free_query_matches_candidate_title_and_artist_without_splitting() {
        for (query, title, artist, candidate_ms, local_seconds) in [
            (
                "爱得干脆 - 吴倩莲",
                "爱得干脆",
                "吴倩莲",
                257_492.0,
                257.493,
            ),
            (
                "难舍难分 - 谭咏麟",
                "难舍难分",
                "谭咏麟",
                275_320.0,
                273.641,
            ),
        ] {
            let song = NeteaseSong {
                id: 1,
                name: title.into(),
                duration: candidate_ms,
                artists: vec![NeteaseArtist {
                    name: artist.into(),
                }],
            };

            assert!(netease_song_matches(
                &song,
                &normalize_match_text(query),
                None,
                Some(Duration::from_secs_f64(local_seconds)),
            ));
        }

        let wrong_artist = NeteaseSong {
            id: 2,
            name: "难舍难分".into(),
            duration: 275_320.0,
            artists: vec![NeteaseArtist {
                name: "其他歌手".into(),
            }],
        };
        assert!(!netease_song_matches(
            &wrong_artist,
            &normalize_match_text("难舍难分 - 谭咏麟"),
            None,
            None,
        ));
    }

    impl LyricsCatalog for ArtistFallbackCatalog {
        fn search(&self, lookup: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
            self.calls.set(self.calls.get() + 1);
            if lookup.artist.is_some() {
                return Ok(vec![]);
            }
            Ok(vec![LyricsCandidate {
                track_name: "昨夜星辰".into(),
                artist_name: "高胜美".into(),
                duration: 199.0,
                synced_lyrics: Some("[00:01.00]昨夜的昨夜的星辰".into()),
            }])
        }
    }

    #[test]
    fn retries_by_title_when_the_tagged_artist_has_no_catalog_result() {
        let catalog = ArtistFallbackCatalog {
            calls: Cell::new(0),
        };
        let lookup = LyricsLookup {
            title: "昨夜星辰".into(),
            artist: Some("龙飘飘".into()),
            duration: Some(std::time::Duration::from_secs_f64(206.43)),
        };
        let mut progress = vec![];

        let candidates = search_candidates(&catalog, &lookup, &mut |event| {
            progress.push(event.to_string());
        })
        .unwrap();

        assert_eq!(catalog.calls.get(), 2);
        assert_eq!(candidates.len(), 1);
        assert!(progress.iter().any(|line| line.contains("by title only")));
    }

    impl LyricsCatalog for FakeCatalog {
        fn search(&self, _: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
            Ok(self.candidates.clone())
        }
    }

    #[test]
    fn downloads_synced_lyrics_into_the_project() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("歌手 - 歌曲.mp3");
        fs::write(&song, b"not real audio").unwrap();
        let root = sandbox.path().join("歌曲");
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: root.clone(),
                song,
                lyrics: None,
                title: Some("歌手 - 歌曲".into()),
            })
            .unwrap();
        let catalog = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "歌曲".into(),
                artist_name: "歌手".into(),
                duration: 180.0,
                synced_lyrics: Some("[00:01.00]第一句\n[00:03.00]第二句".into()),
            }],
        };

        let mut progress = vec![];
        let outcome = download_with_catalog(&mut project, &catalog, &mut |event| {
            progress.push(event.to_string());
        })
        .unwrap();

        assert_eq!(
            outcome,
            LyricsDownload::Downloaded {
                track: "歌曲".into(),
                artist: "歌手".into()
            }
        );
        let relative = project.lyrics().unwrap();
        assert_eq!(relative.as_str(), "lyrics/歌手 - 歌曲.lrc");
        assert!(
            fs::read_to_string(root.join(relative.as_str()))
                .unwrap()
                .contains("[00:03.00]第二句")
        );
        assert!(
            progress
                .iter()
                .any(|message| message.contains("Searching test catalog: 歌手 - 歌曲"))
        );
        assert!(
            progress
                .iter()
                .any(|message| message.contains("Found synced lyrics on test catalog: 歌手 - 歌曲"))
        );
    }

    #[test]
    fn falls_back_when_the_first_catalog_has_no_usable_lyrics() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("fallback.mp3");
        fs::write(&song, b"not real audio").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("Fallback Song".into()),
            })
            .unwrap();
        let primary = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "Fallback Song".into(),
                artist_name: "Primary Artist".into(),
                duration: 180.0,
                synced_lyrics: None,
            }],
        };
        let fallback = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "Fallback Song".into(),
                artist_name: "Fallback Artist".into(),
                duration: 180.0,
                synced_lyrics: Some("[00:01.00]Fallback line".into()),
            }],
        };

        let outcome =
            download_with_catalogs(&mut project, &[&primary, &fallback], &mut |_| {}).unwrap();

        assert!(matches!(outcome, LyricsDownload::Downloaded { .. }));
        assert!(project.lyrics().is_some());
    }

    #[test]
    fn never_calls_the_catalog_when_local_lyrics_exist() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("song.mp3");
        let lyrics = sandbox.path().join("song.lrc");
        fs::write(&song, b"audio").unwrap();
        fs::write(&lyrics, b"[00:01.00]local").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: Some(lyrics),
                title: None,
            })
            .unwrap();
        let catalog = FakeCatalog { candidates: vec![] };

        assert_eq!(
            download_with_catalog(&mut project, &catalog, &mut |_| {}).unwrap(),
            LyricsDownload::AlreadyPresent
        );
    }

    #[test]
    fn downloads_when_configured_lyrics_file_is_missing() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("Missing Lyrics Song.mp3");
        let lyrics = sandbox.path().join("missing.lrc");
        fs::write(&song, b"audio").unwrap();
        fs::write(&lyrics, b"[00:01.00]old local lyrics").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: Some(lyrics),
                title: Some("Missing Lyrics Song".into()),
            })
            .unwrap();
        let configured_lyrics = project.lyrics().unwrap().as_str().to_owned();
        fs::remove_file(project.root().join(&configured_lyrics)).unwrap();
        let catalog = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "Missing Lyrics Song".into(),
                artist_name: "Test Artist".into(),
                duration: 180.0,
                synced_lyrics: Some("[00:01.00]downloaded lyrics".into()),
            }],
        };

        let outcome = download_with_catalog(&mut project, &catalog, &mut |_| {}).unwrap();

        assert!(matches!(outcome, LyricsDownload::Downloaded { .. }));
        let downloaded = project.lyrics().unwrap();
        assert!(project.root().join(downloaded.as_str()).is_file());
    }

    struct FlakyCatalog {
        calls: Cell<usize>,
        candidate: LyricsCandidate,
    }

    impl LyricsCatalog for FlakyCatalog {
        fn search(&self, _: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if call == 0 {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "request timed out").into());
            }
            Ok(vec![self.candidate.clone()])
        }
    }

    #[test]
    fn retries_once_and_reports_the_online_failure() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("Creep.flac");
        fs::write(&song, b"not real audio").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("Creep".into()),
            })
            .unwrap();
        let catalog = FlakyCatalog {
            calls: Cell::new(0),
            candidate: LyricsCandidate {
                track_name: "Creep".into(),
                artist_name: "Radiohead".into(),
                duration: 239.0,
                synced_lyrics: Some("[00:01.00]First line".into()),
            },
        };
        let mut progress = vec![];

        let outcome = download_with_catalog(&mut project, &catalog, &mut |event| {
            progress.push(event.to_string());
        })
        .unwrap();

        assert!(matches!(outcome, LyricsDownload::Downloaded { .. }));
        assert_eq!(catalog.calls.get(), 2);
        assert!(
            progress
                .iter()
                .any(|message| message.contains("retrying (2/2)"))
        );
    }
}
