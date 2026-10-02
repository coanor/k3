use std::{
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};

use k3_core::{LyricsTimeline, Project, ProjectPath, ProjectRepository};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::Accessor,
};
use serde::Deserialize;

const LRCLIB_SEARCH_URL: &str = "https://lrclib.net/api/search";
const NETEASE_SEARCH_URL: &str = "https://music.163.com/api/search/get/web";
const NETEASE_LYRIC_URL: &str = "https://music.163.com/api/song/lyric";
const MAX_DURATION_DIFFERENCE_SECONDS: f64 = 8.0;

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub enum LyricsDownload {
    AlreadyPresent,
    Downloaded { track: String, artist: String },
    NotFound,
}

pub struct LyricsSaved {
    pub track: String,
    pub artist: String,
    pub origin: String,
}

pub enum LyricsSearch {
    AlreadyPresent,
    Candidates(Vec<LyricsChoice>),
    NotFound,
}

#[derive(Clone)]
pub struct LyricsChoice {
    origin: LyricsOrigin,
    lyrics: SelectedLyrics,
}

impl LyricsChoice {
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{} - {} · {:.0}s · {}",
            self.lyrics.artist_name,
            self.lyrics.track_name,
            self.lyrics.duration.max(0.0),
            self.origin.label()
        )
    }

    #[must_use]
    pub fn origin_label(&self) -> String {
        self.origin.label()
    }

    #[must_use]
    pub fn preview_lines(&self, maximum: usize) -> Vec<String> {
        LyricsTimeline::parse(&self.lyrics.synced_lyrics)
            .lines()
            .iter()
            .take(maximum)
            .map(|line| {
                let seconds = line.at.as_secs();
                format!("[{:02}:{:02}] {}", seconds / 60, seconds % 60, line.text)
            })
            .collect()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn for_test(
        source: &'static str,
        track: &str,
        artist: &str,
        duration: f64,
        synced_lyrics: &str,
    ) -> Self {
        Self {
            origin: LyricsOrigin {
                source,
                manual: false,
                fallback: false,
            },
            lyrics: SelectedLyrics {
                track_name: track.into(),
                artist_name: artist.into(),
                duration,
                synced_lyrics: synced_lyrics.into(),
            },
        }
    }
}

#[derive(Clone, Copy)]
struct LyricsOrigin {
    source: &'static str,
    manual: bool,
    fallback: bool,
}

impl LyricsOrigin {
    fn label(self) -> String {
        let route = match (self.manual, self.fallback) {
            (false, false) => "auto",
            (false, true) => "fallback",
            (true, false) => "manual",
            (true, true) => "manual fallback",
        };
        format!("{} · {route}", self.source)
    }
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

/// 搜索工程当前缺少的同步歌词。
///
/// # Errors
///
/// 本地歌词检查或所有在线来源失败时返回错误。
pub fn find_missing_lyrics(
    project: &mut Project,
    netease_fallback: bool,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsSearch, Box<dyn Error>> {
    let lrclib = LrclibCatalog::new();
    if netease_fallback {
        let netease = NeteaseCatalog::new();
        find_with_catalogs(project, &[&lrclib, &netease], true, progress)
    } else {
        find_with_catalogs(project, &[&lrclib], true, progress)
    }
}

/// 使用手动查询词重新搜索同步歌词，不覆盖当前歌词。
///
/// # Errors
///
/// 音频元数据读取或所有在线来源失败时返回错误。
pub fn find_lyrics_again(
    project: &Project,
    query: &str,
    netease_fallback: bool,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsSearch, Box<dyn Error>> {
    let mut searchable = project.clone();
    let lrclib = LrclibCatalog::new();
    if netease_fallback {
        let netease = NeteaseCatalog::new();
        find_with_catalogs_query(
            &mut searchable,
            &[&lrclib, &netease],
            false,
            Some(query),
            progress,
        )
    } else {
        find_with_catalogs_query(&mut searchable, &[&lrclib], false, Some(query), progress)
    }
}

#[must_use]
pub fn default_lyrics_query(project: &Project) -> String {
    lookup_from_audio(project).title
}

/// 返回指定工程用于在线歌词搜索的默认查询词。
///
/// # Errors
///
/// 工程无法读取时返回错误。
pub fn default_project_lyrics_query(project_root: &Path) -> Result<String, Box<dyn Error>> {
    let project = k3_core::FileProjectRepository.open(project_root)?;
    Ok(default_lyrics_query(&project))
}

/// 为指定工程重新搜索同步歌词，不覆盖当前歌词。
///
/// # Errors
///
/// 工程无法读取或所有在线来源均失败时返回错误。
pub fn find_project_lyrics(
    project_root: &Path,
    query: &str,
    netease_fallback: bool,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsSearch, Box<dyn Error>> {
    let project = k3_core::FileProjectRepository.open(project_root)?;
    find_lyrics_again(&project, query, netease_fallback, progress)
}

/// 保存一个在线歌词候选并将其设为工程当前歌词。
///
/// # Errors
///
/// 工程读取、歌词写入或工程保存失败时返回错误。
pub fn save_project_lyrics(
    project_root: &Path,
    choice: LyricsChoice,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsSaved, Box<dyn Error>> {
    let mut project = k3_core::FileProjectRepository.open(project_root)?;
    let (saved, relative_path) = save_lyrics_choice(&project, choice, progress)?;
    project.set_lyrics(relative_path)?;
    k3_core::FileProjectRepository.save(&mut project)?;
    Ok(saved)
}

#[cfg(test)]
fn download_with_catalog(
    project: &mut Project,
    catalog: &dyn LyricsCatalog,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    download_with_catalogs(project, &[catalog], progress)
}

#[cfg(test)]
fn download_with_catalogs(
    project: &mut Project,
    catalogs: &[&dyn LyricsCatalog],
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsDownload, Box<dyn Error>> {
    match find_with_catalogs(project, catalogs, true, progress)? {
        LyricsSearch::AlreadyPresent => Ok(LyricsDownload::AlreadyPresent),
        LyricsSearch::Candidates(mut choices) => {
            let (saved, relative_path) = save_lyrics_choice(project, choices.remove(0), progress)?;
            project.set_lyrics(relative_path)?;
            Ok(LyricsDownload::Downloaded {
                track: saved.track,
                artist: saved.artist,
            })
        }
        LyricsSearch::NotFound => Ok(LyricsDownload::NotFound),
    }
}

fn find_with_catalogs(
    project: &mut Project,
    catalogs: &[&dyn LyricsCatalog],
    check_local: bool,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsSearch, Box<dyn Error>> {
    find_with_catalogs_query(project, catalogs, check_local, None, progress)
}

fn find_with_catalogs_query(
    project: &mut Project,
    catalogs: &[&dyn LyricsCatalog],
    check_local: bool,
    manual_query: Option<&str>,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<LyricsSearch, Box<dyn Error>> {
    progress(&LyricsProgress::CheckingLocal);
    if check_local {
        if let Some(relative_path) = project.lyrics() {
            let configured_path = project.root().join(Path::new(relative_path.as_str()));
            if configured_path.is_file() {
                return Ok(LyricsSearch::AlreadyPresent);
            }
        }

        let relative_path = primary_downloaded_lyrics_path(project)?;
        let destination = project.root().join(Path::new(relative_path.as_str()));
        if destination.is_file() {
            let contents = fs::read_to_string(&destination)?;
            if !LyricsTimeline::parse(&contents).lines().is_empty() {
                project.set_lyrics(relative_path)?;
                return Ok(LyricsSearch::AlreadyPresent);
            }
            return Err(format!(
                "Local lyrics contain no recognizable LRC timeline; file not overwritten: {}",
                destination.display()
            )
            .into());
        }
    }

    let automatic_lookup = lookup_from_audio(project);
    let lookup = manual_query.map_or(automatic_lookup.clone(), |query| LyricsLookup {
        title: query.trim().to_owned(),
        artist: None,
        duration: automatic_lookup.duration,
    });
    let mut successful_search = false;
    let mut failures = Vec::new();
    let mut all_choices = Vec::new();
    for (catalog_index, catalog) in catalogs.iter().enumerate() {
        progress(&LyricsProgress::SearchingOnline {
            source: catalog.name(),
            title: lookup.title.clone(),
            artist: lookup.artist.clone(),
        });
        match search_candidates_with_route(*catalog, &lookup, progress) {
            Ok(search) => {
                successful_search = true;
                let origin = LyricsOrigin {
                    source: catalog.name(),
                    manual: manual_query.is_some(),
                    fallback: catalog_index > 0 || search.title_fallback,
                };
                let choices = select_candidates(search.candidates, lookup.duration)
                    .into_iter()
                    .map(|lyrics| LyricsChoice { origin, lyrics })
                    .collect::<Vec<_>>();
                all_choices.extend(choices);
            }
            Err(error) => failures.push(format!("{}: {error}", catalog.name())),
        }
    }
    if !all_choices.is_empty() {
        return Ok(LyricsSearch::Candidates(all_choices));
    }
    if !successful_search && !failures.is_empty() {
        return Err(format!("All lyric sources failed: {}", failures.join("; ")).into());
    }
    Ok(LyricsSearch::NotFound)
}

/// 将歌词候选原子写入工程目录，但不修改工程的歌词引用。
///
/// # Errors
///
/// 目标路径分配或文件写入失败时返回错误。
pub fn save_lyrics_choice(
    project: &Project,
    choice: LyricsChoice,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<(LyricsSaved, ProjectPath), Box<dyn Error>> {
    let relative_path = available_downloaded_lyrics_path(project)?;
    let destination = project.root().join(Path::new(relative_path.as_str()));
    let candidate = choice.lyrics;
    progress(&LyricsProgress::FoundOnline {
        source: choice.origin.source,
        track: candidate.track_name.clone(),
        artist: candidate.artist_name.clone(),
        duration_seconds: Duration::try_from_secs_f64(candidate.duration.max(0.0))
            .map_or(0, |duration| duration.as_secs()),
    });
    progress(&LyricsProgress::Saving {
        relative_path: relative_path.as_str().to_owned(),
    });
    write_atomically(&destination, &candidate.synced_lyrics)?;
    Ok((
        LyricsSaved {
            track: candidate.track_name,
            artist: candidate.artist_name,
            origin: choice.origin.label(),
        },
        relative_path,
    ))
}

struct CatalogSearch {
    candidates: Vec<LyricsCandidate>,
    title_fallback: bool,
}

fn search_candidates_with_route(
    catalog: &dyn LyricsCatalog,
    lookup: &LyricsLookup,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<CatalogSearch, Box<dyn Error>> {
    let candidates = search_with_retry(catalog, lookup, progress)?;
    if !candidates.is_empty() || lookup.artist.is_none() {
        return Ok(CatalogSearch {
            candidates,
            title_fallback: false,
        });
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
    Ok(CatalogSearch {
        candidates: search_with_retry(catalog, &title_only, progress)?,
        title_fallback: true,
    })
}

#[cfg(test)]
fn search_candidates(
    catalog: &dyn LyricsCatalog,
    lookup: &LyricsLookup,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
    Ok(search_candidates_with_route(catalog, lookup, progress)?.candidates)
}

#[derive(Clone, Debug)]
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

fn select_candidates(
    candidates: Vec<LyricsCandidate>,
    expected_duration: Option<Duration>,
) -> Vec<SelectedLyrics> {
    let mut selected = candidates
        .into_iter()
        .filter_map(|candidate| {
            let duration_difference = expected_duration.map_or(0.0, |expected| {
                (candidate.duration - expected.as_secs_f64()).abs()
            });
            let synced_lyrics = candidate
                .synced_lyrics
                .filter(|lyrics| !lyrics.trim().is_empty())?;
            let parsed = LyricsTimeline::parse(&synced_lyrics);
            if expected_duration.is_some() && duration_difference > MAX_DURATION_DIFFERENCE_SECONDS
                || parsed.lines().is_empty()
            {
                return None;
            }
            Some((
                duration_difference,
                SelectedLyrics {
                    track_name: candidate.track_name,
                    artist_name: candidate.artist_name,
                    duration: candidate.duration,
                    synced_lyrics,
                },
            ))
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.0.total_cmp(&right.0));
    selected
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

#[derive(Clone)]
struct SelectedLyrics {
    track_name: String,
    artist_name: String,
    duration: f64,
    synced_lyrics: String,
}

fn primary_downloaded_lyrics_path(project: &Project) -> Result<ProjectPath, Box<dyn Error>> {
    let source_path = project.source_path();
    let stem = source_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("downloaded");
    Ok(ProjectPath::new(format!("lyrics/{stem}.lrc"))?)
}

fn available_downloaded_lyrics_path(project: &Project) -> Result<ProjectPath, Box<dyn Error>> {
    let primary = primary_downloaded_lyrics_path(project)?;
    if !project.root().join(primary.as_str()).exists() {
        return Ok(primary);
    }
    let source_path = project.source_path();
    let stem = source_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("downloaded");
    for version in 2..=10_000 {
        let candidate = ProjectPath::new(format!("lyrics/{stem}-{version}.lrc"))?;
        if !project.root().join(candidate.as_str()).exists() {
            return Ok(candidate);
        }
    }
    Err("too many downloaded lyric versions in this project".into())
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
        LyricsCandidate, LyricsCatalog, LyricsDownload, LyricsLookup, LyricsSearch, NeteaseArtist,
        NeteaseSong, download_with_catalog, download_with_catalogs, find_with_catalogs,
        find_with_catalogs_query, netease_song_matches, normalize_match_text, save_lyrics_choice,
        save_project_lyrics, search_candidates,
    };
    use k3_core::{CreateProject, FileProjectRepository, ProjectPath, ProjectRepository};
    use std::{
        cell::{Cell, RefCell},
        error::Error,
        fs, io,
        time::Duration,
    };

    struct FakeCatalog {
        candidates: Vec<LyricsCandidate>,
    }

    struct ArtistFallbackCatalog {
        calls: Cell<usize>,
    }

    struct CapturingCatalog {
        lookup: RefCell<Option<(String, Option<String>)>>,
        candidates: Vec<LyricsCandidate>,
    }

    impl LyricsCatalog for CapturingCatalog {
        fn search(&self, lookup: &LyricsLookup) -> Result<Vec<LyricsCandidate>, Box<dyn Error>> {
            self.lookup
                .replace(Some((lookup.title.clone(), lookup.artist.clone())));
            Ok(self.candidates.clone())
        }
    }

    #[test]
    fn manual_query_is_not_split_and_is_labeled() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("old-name.flac");
        fs::write(&song, b"audio").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("old-name".into()),
            })
            .unwrap();
        let catalog = CapturingCatalog {
            lookup: RefCell::new(None),
            candidates: vec![LyricsCandidate {
                track_name: "难舍难分".into(),
                artist_name: "谭咏麟".into(),
                duration: 275.0,
                synced_lyrics: Some("[00:01.00]忘不了你眼中那闪烁的泪光".into()),
            }],
        };

        let result = find_with_catalogs_query(
            &mut project,
            &[&catalog],
            false,
            Some("难舍难分 - 谭咏麟"),
            &mut |_| {},
        )
        .unwrap();
        let LyricsSearch::Candidates(choices) = result else {
            panic!("expected candidates")
        };

        assert_eq!(
            catalog.lookup.borrow().clone(),
            Some(("难舍难分 - 谭咏麟".into(), None))
        );
        assert_eq!(choices[0].origin_label(), "test catalog · manual");
        assert!(choices[0].preview_lines(3)[0].contains("忘不了你"));
    }

    #[test]
    fn keeps_all_duration_matched_lyrics_for_user_selection() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("同名歌曲.flac");
        fs::write(&song, b"audio").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("同名歌曲".into()),
            })
            .unwrap();
        let catalog = FakeCatalog {
            candidates: vec![
                LyricsCandidate {
                    track_name: "同名歌曲".into(),
                    artist_name: "歌手甲".into(),
                    duration: 180.0,
                    synced_lyrics: Some("[00:01.00]版本甲".into()),
                },
                LyricsCandidate {
                    track_name: "同名歌曲".into(),
                    artist_name: "歌手乙".into(),
                    duration: 184.0,
                    synced_lyrics: Some("[00:01.00]版本乙".into()),
                },
            ],
        };

        let result = find_with_catalogs(&mut project, &[&catalog], true, &mut |_| {}).unwrap();
        let LyricsSearch::Candidates(candidates) = result else {
            panic!("expected candidates")
        };

        assert_eq!(candidates.len(), 2);
        assert!(project.lyrics().is_none());
        assert_eq!(
            fs::read_dir(project.root().join("lyrics")).unwrap().count(),
            0
        );
    }

    #[test]
    fn re_search_ignores_existing_lyrics_and_keeps_them_until_a_choice_is_saved() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("同名歌曲.flac");
        fs::write(&song, b"audio").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("同名歌曲".into()),
            })
            .unwrap();
        let old_relative = ProjectPath::new("lyrics/同名歌曲.lrc").unwrap();
        let old_path = project.root().join(old_relative.as_str());
        fs::write(&old_path, "[00:01.00]旧歌词").unwrap();
        project.set_lyrics(old_relative.clone()).unwrap();
        let catalog = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "同名歌曲".into(),
                artist_name: "正确歌手".into(),
                duration: 182.0,
                synced_lyrics: Some("[00:01.00]新歌词".into()),
            }],
        };

        let result = find_with_catalogs(&mut project, &[&catalog], false, &mut |_| {}).unwrap();
        let LyricsSearch::Candidates(mut choices) = result else {
            panic!("expected replacement candidates")
        };
        assert_eq!(fs::read_to_string(&old_path).unwrap(), "[00:01.00]旧歌词");
        assert_eq!(project.lyrics(), Some(&old_relative));

        let (_, replacement) =
            save_lyrics_choice(&project, choices.remove(0), &mut |_| {}).unwrap();
        assert_eq!(replacement.as_str(), "lyrics/同名歌曲-2.lrc");
        assert_eq!(fs::read_to_string(&old_path).unwrap(), "[00:01.00]旧歌词");
        assert!(
            fs::read_to_string(project.root().join(replacement.as_str()))
                .unwrap()
                .contains("新歌词")
        );
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
    fn gui_workflow_saves_choice_and_updates_project() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("gui-song.mp3");
        fs::write(&song, b"audio").unwrap();
        let root = sandbox.path().join("project");
        FileProjectRepository
            .create(CreateProject {
                root: root.clone(),
                song,
                lyrics: None,
                title: Some("GUI Song".into()),
            })
            .unwrap();
        let choice = super::LyricsChoice::for_test(
            "GUI test",
            "GUI Song",
            "Singer",
            180.0,
            "[00:01.00]first line\n[00:03.00]second line",
        );

        let saved = save_project_lyrics(&root, choice, &mut |_| {}).unwrap();
        let project = FileProjectRepository.open(&root).unwrap();
        let relative = project.lyrics().expect("lyrics should be attached");

        assert_eq!(saved.track, "GUI Song");
        assert_eq!(saved.origin, "GUI test · auto");
        assert!(
            fs::read_to_string(root.join(relative.as_str()))
                .unwrap()
                .contains("second line")
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

        let search =
            find_with_catalogs(&mut project, &[&primary, &fallback], true, &mut |_| {}).unwrap();
        let LyricsSearch::Candidates(choices) = search else {
            panic!("expected fallback candidates")
        };
        assert_eq!(choices[0].origin_label(), "test catalog · fallback");

        let outcome =
            download_with_catalogs(&mut project, &[&primary, &fallback], &mut |_| {}).unwrap();

        assert!(matches!(outcome, LyricsDownload::Downloaded { .. }));
        assert!(project.lyrics().is_some());
    }

    #[test]
    fn includes_fallback_candidates_when_primary_already_has_a_result() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("song.mp3");
        fs::write(&song, b"audio").unwrap();
        let mut project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("Song".into()),
            })
            .unwrap();
        let primary = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "Unexpected version".into(),
                artist_name: "Primary artist".into(),
                duration: 180.0,
                synced_lyrics: Some("[00:01.00]primary lyrics".into()),
            }],
        };
        let fallback = FakeCatalog {
            candidates: vec![LyricsCandidate {
                track_name: "Expected version".into(),
                artist_name: "Fallback artist".into(),
                duration: 180.0,
                synced_lyrics: Some("[00:01.00]fallback lyrics".into()),
            }],
        };

        let result =
            find_with_catalogs(&mut project, &[&primary, &fallback], false, &mut |_| {}).unwrap();
        let LyricsSearch::Candidates(choices) = result else {
            panic!("expected candidates")
        };

        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].origin_label(), "test catalog · auto");
        assert_eq!(choices[1].origin_label(), "test catalog · fallback");
        assert!(choices[1].preview_lines(1)[0].contains("fallback lyrics"));
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
