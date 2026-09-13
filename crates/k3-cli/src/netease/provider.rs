use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{
    AccountProfile, AudioSource, AuthorizedLogin, LoginPoll, LoginTicket, NeteaseError, Quality,
    Song, SongPage, weapi, write_bytes,
};

pub(super) const NETEASE_BASE_URL: &str = "https://music.163.com";
pub(super) const NETEASE_USER_AGENT: &str = "Mozilla/5.0 (K3 experimental NetEase source)";
const NETEASE_WEAPI_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) NeteaseMusicDesktop/3.0.12.2443";
const NETEASE_WEAPI_COOKIE_CONTEXT: &str = "channel=appstore; ntes_kaola_ad=1; WEVNSM=1.0; \
appver=3.0.12; os=osx; osver=15.3.2; mode=MacBookPro16,1; _iuqxldmzr_=33; \
__remember_me=true";

pub(super) trait NeteaseProvider: Send + Sync {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError>;
    fn poll_login(&self, key: &str) -> Result<LoginPoll, NeteaseError>;
    fn account(&self, cookie: &str) -> Result<AccountProfile, NeteaseError>;
    fn search(
        &self,
        _cookie: &str,
        _query: &str,
        _offset: usize,
        _limit: usize,
    ) -> Result<SongPage, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider does not support search".into(),
        ))
    }
    fn liked_songs(&self, _cookie: &str, _user_id: u64) -> Result<Vec<Song>, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider does not support liked songs".into(),
        ))
    }
    fn audio_source(
        &self,
        _cookie: &str,
        _song_id: u64,
        _quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider does not support downloads".into(),
        ))
    }
    fn fetch_bytes(
        &self,
        _url: &str,
        _cookie: Option<&str>,
        _maximum: usize,
    ) -> Result<Vec<u8>, NeteaseError> {
        Err(NeteaseError::Protocol(
            "this NetEase provider cannot fetch media".into(),
        ))
    }
    fn fetch_audio(
        &self,
        url: &str,
        destination: &Path,
        maximum: usize,
        cancellation: &AtomicBool,
    ) -> Result<u64, NeteaseError> {
        if cancellation.load(Ordering::Acquire) {
            return Err(NeteaseError::Cancelled);
        }
        let audio = self.fetch_bytes(url, None, maximum)?;
        if cancellation.load(Ordering::Acquire) {
            return Err(NeteaseError::Cancelled);
        }
        write_bytes(destination, &audio)?;
        Ok(audio.len() as u64)
    }
}

pub(super) struct WebNeteaseProvider {
    pub(super) agent: ureq::Agent,
    base_url: String,
}

impl WebNeteaseProvider {
    pub(super) fn new() -> Self {
        Self::at_base_url(NETEASE_BASE_URL)
    }

    pub(super) fn at_base_url(base_url: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.into(),
        }
    }
}

#[derive(Deserialize)]
struct UniqueKeyResponse {
    code: i32,
    unikey: Option<String>,
}

#[derive(Deserialize)]
struct LoginPollResponse {
    code: i32,
    cookie: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct AccountResponse {
    profile: Option<WebAccountProfile>,
}

#[derive(Deserialize)]
struct WebAccountProfile {
    #[serde(rename = "userId")]
    user_id: u64,
    nickname: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    code: i32,
    result: Option<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<SearchSong>,
    #[serde(rename = "songCount", default)]
    song_count: usize,
}

#[derive(Deserialize)]
struct SearchSong {
    id: u64,
}

#[derive(Deserialize)]
struct UserPlaylistsResponse {
    code: i32,
    #[serde(default)]
    playlist: Vec<WebPlaylist>,
}

#[derive(Deserialize)]
struct WebPlaylist {
    id: u64,
    #[serde(rename = "specialType")]
    special_type: i32,
}

#[derive(Deserialize)]
struct PlaylistDetailResponse {
    code: i32,
    playlist: Option<PlaylistDetail>,
}

#[derive(Deserialize)]
struct PlaylistDetail {
    #[serde(rename = "trackIds", default)]
    track_ids: Vec<TrackId>,
}

#[derive(Deserialize)]
struct TrackId {
    id: u64,
}

#[derive(Deserialize)]
struct SongDetailResponse {
    code: i32,
    #[serde(default)]
    songs: Vec<WebSong>,
}

#[derive(Deserialize)]
struct WebSong {
    id: u64,
    name: String,
    status: i32,
    #[serde(default)]
    artists: Vec<WebArtist>,
    album: WebAlbum,
    #[serde(rename = "hrMusic")]
    hi_res: Option<WebAudioQuality>,
    #[serde(rename = "sqMusic")]
    lossless: Option<WebAudioQuality>,
    #[serde(rename = "hMusic")]
    high: Option<WebAudioQuality>,
    #[serde(rename = "mMusic")]
    medium: Option<WebAudioQuality>,
}

#[derive(Deserialize)]
struct WebArtist {
    name: String,
}

#[derive(Deserialize)]
struct WebAlbum {
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
}

#[derive(Deserialize)]
struct WebAudioQuality {}

#[derive(Deserialize)]
struct DownloadUrlResponse {
    code: i32,
    data: Option<WebAudioSource>,
}

#[derive(Serialize)]
struct DownloadUrlRequest<'a> {
    id: &'a str,
    br: &'a str,
}

#[derive(Deserialize)]
struct WebAudioSource {
    #[serde(default)]
    code: i32,
    url: Option<String>,
    #[serde(rename = "type")]
    extension: Option<String>,
    #[serde(default)]
    size: u64,
    level: Option<Quality>,
}

impl From<WebSong> for Song {
    fn from(song: WebSong) -> Self {
        let max_quality = if song.hi_res.is_some() {
            Quality::HiRes
        } else if song.lossless.is_some() {
            Quality::Lossless
        } else if song.high.is_some() {
            Quality::ExHigh
        } else if song.medium.is_some() {
            Quality::Higher
        } else {
            Quality::Standard
        };
        Self {
            id: song.id,
            title: song.name,
            artists: song.artists.into_iter().map(|artist| artist.name).collect(),
            album: song.album.name,
            cover_url: song.album.pic_url,
            max_quality,
            available: song.status == 0,
        }
    }
}

impl NeteaseProvider for WebNeteaseProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        let timestamp = now_millis().to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/login/qrcode/unikey"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .query("type", "1")
            .query("timestamp", &timestamp)
            .call()
            .map_err(http_error)?;
        let body: UniqueKeyResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "QR login initialization returned code {}",
                body.code
            )));
        }
        let key = body
            .unikey
            .ok_or_else(|| NeteaseError::Protocol("QR login key is missing".into()))?;
        let qr_url = format!("{NETEASE_BASE_URL}/login?codekey={key}");
        Ok(LoginTicket::new(key, qr_url))
    }

    fn poll_login(&self, key: &str) -> Result<LoginPoll, NeteaseError> {
        let timestamp = now_millis().to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/login/qrcode/client/login"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .query("key", key)
            .query("type", "1")
            .query("timestamp", &timestamp)
            .call()
            .map_err(http_error)?;
        let body: LoginPollResponse = response.body_mut().read_json().map_err(body_error)?;
        match body.code {
            800 => Ok(LoginPoll::Expired),
            801 => Ok(LoginPoll::WaitingScan),
            802 => Ok(LoginPoll::WaitingConfirmation),
            803 => body.cookie.map_or_else(
                || {
                    Err(NeteaseError::Protocol(
                        "authorized QR login did not return a cookie".into(),
                    ))
                },
                |cookie| Ok(LoginPoll::Authorized(AuthorizedLogin { cookie })),
            ),
            code => Err(NeteaseError::Protocol(format!(
                "QR login returned code {code}: {}",
                body.message.unwrap_or_else(|| "unknown response".into())
            ))),
        }
    }

    fn account(&self, cookie: &str) -> Result<AccountProfile, NeteaseError> {
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/nuser/account/get"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .call()
            .map_err(http_error)?;
        let body: AccountResponse = response.body_mut().read_json().map_err(body_error)?;
        let profile = body.profile.ok_or(NeteaseError::LoginRequired)?;
        Ok(AccountProfile {
            user_id: profile.user_id,
            nickname: profile.nickname,
        })
    }

    fn search(
        &self,
        cookie: &str,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<SongPage, NeteaseError> {
        let offset_text = offset.to_string();
        let limit_text = limit.to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/search/get/web"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .query("s", query)
            .query("type", "1")
            .query("offset", &offset_text)
            .query("limit", &limit_text)
            .query("total", "true")
            .call()
            .map_err(http_error)?;
        let body: SearchResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "song search returned code {}",
                body.code
            )));
        }
        let result = body.result.unwrap_or(SearchResult {
            songs: Vec::new(),
            song_count: 0,
        });
        let ids = result
            .songs
            .into_iter()
            .map(|song| song.id)
            .collect::<Vec<_>>();
        Ok(SongPage {
            songs: self.song_details(cookie, &ids)?,
            total: result.song_count,
            offset,
        })
    }

    fn liked_songs(&self, cookie: &str, user_id: u64) -> Result<Vec<Song>, NeteaseError> {
        let user_id = user_id.to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/user/playlist/"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .query("uid", &user_id)
            .query("offset", "0")
            .query("limit", "100")
            .call()
            .map_err(http_error)?;
        let body: UserPlaylistsResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "user playlists returned code {}",
                body.code
            )));
        }
        let playlist_id = body
            .playlist
            .into_iter()
            .find(|playlist| playlist.special_type == 5)
            .map(|playlist| playlist.id)
            .ok_or_else(|| NeteaseError::Protocol("liked songs playlist is missing".into()))?;
        let playlist_id = playlist_id.to_string();
        let mut response = self
            .agent
            .get(format!("{NETEASE_BASE_URL}/api/v6/playlist/detail"))
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", cookie)
            .query("id", &playlist_id)
            .query("n", "100000")
            .query("s", "0")
            .call()
            .map_err(http_error)?;
        let body: PlaylistDetailResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "liked songs playlist returned code {}",
                body.code
            )));
        }
        let ids = body
            .playlist
            .ok_or_else(|| NeteaseError::Protocol("liked songs playlist is empty".into()))?
            .track_ids
            .into_iter()
            .map(|track| track.id)
            .collect::<Vec<_>>();
        self.song_details(cookie, &ids)
    }

    fn audio_source(
        &self,
        cookie: &str,
        song_id: u64,
        quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        let song_id_text = song_id.to_string();
        let bitrate = quality.download_bitrate().to_string();
        let encrypted = weapi::encrypt(&DownloadUrlRequest {
            id: &song_id_text,
            br: &bitrate,
        })
        .map_err(|error| {
            NeteaseError::Protocol(format!("cannot encrypt WEAPI request: {error}"))
        })?;
        let mut request = self
            .agent
            .post(format!("{}/weapi/song/enhance/download/url", self.base_url))
            .header("User-Agent", NETEASE_WEAPI_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .header("Cookie", weapi_cookie_header(cookie));
        if let Some(csrf) = cookie_header_value(cookie, "__csrf") {
            request = request.query("csrf_token", csrf);
        }
        let mut response = request
            .send_form([
                ("params", encrypted.params.as_str()),
                ("encSecKey", encrypted.enc_sec_key.as_str()),
            ])
            .map_err(http_error)?;
        let body: DownloadUrlResponse = response.body_mut().read_json().map_err(body_error)?;
        if body.code == 301 {
            return Err(NeteaseError::LoginRequired);
        }
        if body.code != 200 {
            return Err(NeteaseError::Protocol(format!(
                "audio URL returned code {}",
                body.code
            )));
        }
        let Some(audio) = body.data else {
            return Ok(None);
        };
        if !matches!(audio.code, 0 | 200) {
            return Ok(None);
        }
        let Some(url) = audio.url else {
            return Ok(None);
        };
        Ok(Some(AudioSource {
            url,
            quality: audio.level.unwrap_or(quality),
            extension: audio.extension.unwrap_or_else(|| "mp3".into()),
            size: audio.size,
        }))
    }

    fn fetch_bytes(
        &self,
        url: &str,
        cookie: Option<&str>,
        maximum: usize,
    ) -> Result<Vec<u8>, NeteaseError> {
        let mut request = self
            .agent
            .get(url)
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let mut response = request.call().map_err(http_error)?;
        response
            .body_mut()
            .with_config()
            .limit(maximum as u64)
            .read_to_vec()
            .map_err(body_error)
    }

    fn fetch_audio(
        &self,
        url: &str,
        destination: &Path,
        maximum: usize,
        cancellation: &AtomicBool,
    ) -> Result<u64, NeteaseError> {
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", NETEASE_USER_AGENT)
            .header("Referer", NETEASE_BASE_URL)
            .call()
            .map_err(http_error)?;
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(maximum.saturating_add(1) as u64)
            .reader();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(destination)?;
        let mut buffer = vec![0_u8; 64 * 1_024].into_boxed_slice();
        let mut downloaded = 0_u64;
        loop {
            if cancellation.load(Ordering::Acquire) {
                return Err(NeteaseError::Cancelled);
            }
            let count = io::Read::read(&mut reader, &mut buffer)
                .map_err(|error| NeteaseError::Http(error.to_string()))?;
            if count == 0 {
                break;
            }
            downloaded = downloaded.saturating_add(count as u64);
            if downloaded > maximum as u64 {
                return Err(NeteaseError::Protocol(format!(
                    "media response exceeded {maximum} bytes"
                )));
            }
            file.write_all(&buffer[..count])?;
        }
        file.sync_all()?;
        Ok(downloaded)
    }
}

fn cookie_header_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').find_map(|part| {
        let (candidate, value) = part.trim().split_once('=')?;
        (candidate == name).then_some(value)
    })
}

pub(super) fn weapi_cookie_header(cookie: &str) -> String {
    let mut header = cookie.trim().trim_end_matches(';').to_owned();
    for default in NETEASE_WEAPI_COOKIE_CONTEXT.split(';').map(str::trim) {
        let name = default.split_once('=').map_or(default, |(name, _)| name);
        if cookie_header_value(cookie, name).is_none() {
            if !header.is_empty() {
                header.push_str("; ");
            }
            header.push_str(default);
        }
    }
    header
}

impl WebNeteaseProvider {
    fn song_details(&self, cookie: &str, ids: &[u64]) -> Result<Vec<Song>, NeteaseError> {
        let mut songs_by_id = BTreeMap::new();
        for chunk in ids.chunks(200) {
            let ids_json = serde_json::to_string(chunk)?;
            let mut response = self
                .agent
                .get(format!("{NETEASE_BASE_URL}/api/song/detail"))
                .header("User-Agent", NETEASE_USER_AGENT)
                .header("Referer", NETEASE_BASE_URL)
                .header("Cookie", cookie)
                .query("ids", &ids_json)
                .call()
                .map_err(http_error)?;
            let body: SongDetailResponse = response.body_mut().read_json().map_err(body_error)?;
            if body.code == 301 {
                return Err(NeteaseError::LoginRequired);
            }
            if body.code != 200 {
                return Err(NeteaseError::Protocol(format!(
                    "song details returned code {}",
                    body.code
                )));
            }
            for song in body.songs {
                songs_by_id.insert(song.id, song.into());
            }
        }
        Ok(ids.iter().filter_map(|id| songs_by_id.remove(id)).collect())
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn http_error(error: ureq::Error) -> NeteaseError {
    match error {
        ureq::Error::StatusCode(code) if code < 500 && !matches!(code, 408 | 429) => {
            NeteaseError::Protocol(format!("request returned HTTP {code}"))
        }
        error => NeteaseError::Http(error.to_string()),
    }
}

fn body_error(error: ureq::Error) -> NeteaseError {
    let message = error.to_string();
    drop(error);
    NeteaseError::Http(message)
}
