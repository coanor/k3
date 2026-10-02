use std::{
    collections::BTreeMap,
    fmt, fs,
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{NeteaseError, write_private_json};

pub(super) struct DownloadPaths {
    pub(super) filename: String,
    pub(super) previous: Option<PathBuf>,
    pub(super) destination: PathBuf,
    pub(super) temporary: PathBuf,
    pub(super) backup: PathBuf,
}

impl DownloadPaths {
    pub(super) fn new(
        output_root: &Path,
        song: &Song,
        source: &AudioSource,
        decision: &DownloadDecision,
    ) -> Self {
        let base_filename = download_filename(
            song.primary_artist(),
            &song.title,
            &source.extension,
            song.id,
        );
        let mut filename = base_filename.clone();
        let previous = match decision {
            DownloadDecision::Upgrade(path) => Some(output_root.join(path)),
            DownloadDecision::Download | DownloadDecision::Skip => None,
        };
        let mut destination = output_root.join(&filename);
        if !download_destination_available(&destination, previous.as_ref()) {
            let filename_with_id = filename_with_id(&base_filename, song.id);
            filename.clone_from(&filename_with_id);
            destination = output_root.join(&filename);
            let mut collision = 2;
            while !download_destination_available(&destination, previous.as_ref()) {
                filename = filename_with_collision_index(&filename_with_id, collision);
                destination = output_root.join(&filename);
                collision += 1;
            }
        }
        Self {
            filename,
            previous,
            destination,
            temporary: output_root.join(format!(
                ".{}-{}.part.{}",
                std::process::id(),
                song.id,
                sanitize_extension(&source.extension)
            )),
            backup: output_root.join(format!(".{}-{}.backup", std::process::id(), song.id)),
        }
    }
}

fn download_destination_available(destination: &Path, previous: Option<&PathBuf>) -> bool {
    !destination.exists() || previous.is_some_and(|path| path == destination)
}

pub(super) fn commit_download(
    index: &mut DownloadIndex,
    song: &Song,
    source: &AudioSource,
    paths: &DownloadPaths,
) -> Result<(), NeteaseError> {
    let backed_up = paths.destination.exists();
    if backed_up {
        let _ = fs::remove_file(&paths.backup);
        fs::rename(&paths.destination, &paths.backup)?;
    }
    if let Err(error) = fs::rename(&paths.temporary, &paths.destination) {
        if backed_up {
            let _ = fs::rename(&paths.backup, &paths.destination);
        }
        return Err(error.into());
    }

    index.record(DownloadRecord {
        song_id: song.id,
        quality: source.quality,
        path: PathBuf::from(&paths.filename),
    });
    if let Err(error) = index.save() {
        let _ = fs::remove_file(&paths.destination);
        if backed_up {
            let _ = fs::rename(&paths.backup, &paths.destination);
        }
        return Err(error);
    }
    if backed_up {
        let _ = fs::remove_file(&paths.backup);
    }
    if let Some(previous) = &paths.previous
        && *previous != paths.destination
    {
        let _ = fs::remove_file(previous);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Song {
    pub id: u64,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub cover_url: Option<String>,
    pub max_quality: Quality,
    pub available: bool,
}

impl Song {
    pub fn primary_artist(&self) -> &str {
        self.artists
            .first()
            .map_or("Unknown Artist", String::as_str)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SongPage {
    pub songs: Vec<Song>,
    pub total: usize,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Standard,
    Higher,
    ExHigh,
    Lossless,
    HiRes,
    Jyeffect,
    Sky,
    Jymaster,
}

impl Quality {
    pub const HIGHEST_FIRST: [Self; 8] = [
        Self::Jymaster,
        Self::Sky,
        Self::Jyeffect,
        Self::HiRes,
        Self::Lossless,
        Self::ExHigh,
        Self::Higher,
        Self::Standard,
    ];

    pub const fn as_level(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Higher => "higher",
            Self::ExHigh => "exhigh",
            Self::Lossless => "lossless",
            Self::HiRes => "hires",
            Self::Jyeffect => "jyeffect",
            Self::Sky => "sky",
            Self::Jymaster => "jymaster",
        }
    }

    pub(super) const fn download_bitrate(self) -> u64 {
        match self {
            Self::Standard => 128_000,
            Self::Higher => 192_000,
            Self::ExHigh => 320_000,
            Self::Lossless => 999_000,
            Self::HiRes | Self::Jyeffect | Self::Sky | Self::Jymaster => 9_999_999,
        }
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_level())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioSource {
    pub(super) url: String,
    pub(super) quality: Quality,
    pub(super) extension: String,
    pub(super) size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadOutcome {
    Downloaded { path: PathBuf, quality: Quality },
    Skipped { song_id: u64, quality: Quality },
    Unavailable { song_id: u64, reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DownloadRecord {
    pub song_id: u64,
    pub quality: Quality,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadDecision {
    Download,
    Skip,
    Upgrade(PathBuf),
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct DownloadIndexData {
    #[serde(default)]
    songs: BTreeMap<u64, DownloadRecord>,
}

#[derive(Debug)]
pub struct DownloadIndex {
    path: PathBuf,
    data: DownloadIndexData,
}

impl DownloadIndex {
    pub fn load(path: &Path) -> Result<Self, NeteaseError> {
        let data = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => DownloadIndexData::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path: path.to_path_buf(),
            data,
        })
    }

    pub fn decision(&self, song_id: u64, quality: Quality) -> DownloadDecision {
        let Some(existing) = self.data.songs.get(&song_id) else {
            return DownloadDecision::Download;
        };
        let Some(filename) = existing.path.file_name() else {
            return DownloadDecision::Download;
        };
        if existing.path != Path::new(filename)
            || !self
                .path
                .parent()
                .is_some_and(|parent| parent.join(filename).is_file())
        {
            return DownloadDecision::Download;
        }
        if existing.quality >= quality {
            DownloadDecision::Skip
        } else {
            DownloadDecision::Upgrade(existing.path.clone())
        }
    }

    pub fn cached_path(&self, song_id: u64) -> Option<PathBuf> {
        let record = self.data.songs.get(&song_id)?;
        let filename = record.path.file_name()?;
        if record.path != Path::new(filename) {
            return None;
        }
        let path = self.path.parent()?.join(filename);
        path.is_file().then_some(path)
    }

    pub fn record(&mut self, record: DownloadRecord) {
        self.data.songs.insert(record.song_id, record);
    }

    pub fn save(&self) -> Result<(), NeteaseError> {
        write_private_json(&self.path, &self.data)
    }
}

pub fn download_filename(primary_artist: &str, title: &str, extension: &str, _id: u64) -> String {
    let artist = sanitize_filename_part(primary_artist);
    let title = sanitize_filename_part(title);
    let extension = sanitize_extension(extension);
    format!("{artist}-{title}.{extension}")
}

fn sanitize_extension(value: &str) -> String {
    let extension = value
        .trim_start_matches('.')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if extension.is_empty() {
        "mp3".into()
    } else {
        extension
    }
}

fn filename_with_id(filename: &str, id: u64) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Song");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3");
    format!("{stem}[{id}].{extension}")
}

fn filename_with_collision_index(filename: &str, collision: usize) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Song");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3");
    format!("{stem}-{collision}.{extension}")
}

pub(super) fn retry_transient<T>(
    mut operation: impl FnMut() -> Result<T, NeteaseError>,
) -> Result<T, NeteaseError> {
    let mut delay = std::time::Duration::from_millis(200);
    for attempt in 0..4 {
        match operation() {
            Ok(value) => return Ok(value),
            Err(NeteaseError::Http(_)) if attempt < 3 => {
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the retry loop always returns")
}

pub(super) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), NeteaseError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn tag_audio(
    path: &Path,
    song: &Song,
    cover: Option<&[u8]>,
) -> Result<(), NeteaseError> {
    use lofty::{
        config::WriteOptions,
        file::{AudioFile, TaggedFileExt},
        picture::{Picture, PictureType},
        prelude::{Accessor, ItemKey},
        tag::{ItemValue, Tag, TagItem},
    };

    let mut tagged =
        lofty::read_from_path(path).map_err(|error| NeteaseError::Metadata(error.to_string()))?;
    let tag_type = tagged.primary_tag_type();
    if tagged.tag(tag_type).is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .tag_mut(tag_type)
        .ok_or_else(|| NeteaseError::Metadata("audio format has no writable tag".into()))?;
    tag.set_title(song.title.clone());
    tag.set_artist(song.artists.join(", "));
    tag.set_album(song.album.clone());
    tag.insert(TagItem::new(
        ItemKey::CatalogNumber,
        ItemValue::Text(format!("netease:{}", song.id)),
    ));
    if let Some(cover) = cover {
        let mut picture = Picture::from_reader(&mut std::io::Cursor::new(cover))
            .map_err(|error| NeteaseError::Metadata(error.to_string()))?;
        picture.set_pic_type(PictureType::CoverFront);
        tag.push_picture(picture);
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| NeteaseError::Metadata(error.to_string()))
}

fn sanitize_filename_part(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_end_matches(['.', ' ']);
    if sanitized.is_empty() {
        "Unknown".to_owned()
    } else {
        sanitized.to_owned()
    }
}
