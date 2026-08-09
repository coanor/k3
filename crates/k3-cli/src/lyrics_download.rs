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
        title: String,
        artist: Option<String>,
    },
    RetryingOnline {
        reason: String,
    },
    FoundOnline {
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
            Self::CheckingLocal => formatter.write_str("正在检查本地歌词……"),
            Self::SearchingOnline { title, artist } => {
                if let Some(artist) = artist {
                    write!(
                        formatter,
                        "本地没有歌词，正在从 LRCLIB 搜索：{artist} - {title}……"
                    )
                } else {
                    write!(formatter, "本地没有歌词，正在从 LRCLIB 搜索：{title}……")
                }
            }
            Self::RetryingOnline { reason } => {
                write!(formatter, "LRCLIB 请求失败：{reason}；正在重试（2/2）……")
            }
            Self::FoundOnline {
                track,
                artist,
                duration_seconds,
            } => write!(
                formatter,
                "找到同步歌词：{artist} - {track}（{duration_seconds} 秒）"
            ),
            Self::Saving { relative_path } => {
                write!(formatter, "正在保存歌词：{relative_path}")
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
/// Returns an error when LRCLIB is unavailable or the selected lyrics cannot be persisted.
pub fn download_missing_lyrics(
    project: &mut Project,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    let catalog = LrclibCatalog::new();
    download_with_catalog(project, &catalog, progress)
}

fn download_with_catalog(
    project: &mut Project,
    catalog: &dyn LyricsCatalog,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    progress(&LyricsProgress::CheckingLocal);
    if project.lyrics().is_some() {
        return Ok(LyricsDownload::AlreadyPresent);
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
            "本地歌词文件不含可识别的 LRC 时间轴，未覆盖：{}",
            destination.display()
        )
        .into());
    }

    let lookup = lookup_from_audio(project);
    progress(&LyricsProgress::SearchingOnline {
        title: lookup.title.clone(),
        artist: lookup.artist.clone(),
    });
    let candidates = search_with_retry(catalog, &lookup, progress)?;
    let Some(candidate) = select_candidate(candidates, lookup.duration) else {
        return Ok(LyricsDownload::NotFound);
    };
    if LyricsTimeline::parse(&candidate.synced_lyrics)
        .lines()
        .is_empty()
    {
        return Ok(LyricsDownload::NotFound);
    }

    progress(&LyricsProgress::FoundOnline {
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
                reason: first_error.to_string(),
            });
            catalog.search(lookup).map_err(|retry_error| {
                format!("LRCLIB 两次请求均失败；首次：{first_error}；重试：{retry_error}").into()
            })
        }
    }
}

impl LyricsCatalog for LrclibCatalog {
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
        LyricsCandidate, LyricsCatalog, LyricsDownload, LyricsLookup, download_with_catalog,
    };
    use k3_core::{CreateProject, FileProjectRepository, ProjectRepository};
    use std::{cell::Cell, error::Error, fs, io};

    struct FakeCatalog {
        candidates: Vec<LyricsCandidate>,
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
                .any(|message| message.contains("正在从 LRCLIB 搜索：歌手 - 歌曲"))
        );
        assert!(
            progress
                .iter()
                .any(|message| message.contains("找到同步歌词：歌手 - 歌曲"))
        );
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
                .any(|message| message.contains("正在重试（2/2）"))
        );
    }
}
