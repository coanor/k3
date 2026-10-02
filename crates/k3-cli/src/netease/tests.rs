use super::{
    AccountProfile, AudioSource, AuthorizedLogin, BrowserCookie, ChromeCookieProfile,
    ChromeCookieSource, DownloadDecision, DownloadIndex, DownloadOutcome, DownloadPaths,
    DownloadRecord, LoginPoll, LoginStatus, LoginTicket, NeteaseClient, NeteaseError,
    NeteaseProvider, NeteaseSession, Quality, SessionStore, Song, SongPage, download_filename,
};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

struct LoginProvider;

impl NeteaseProvider for LoginProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        Ok(LoginTicket::new("ticket", "https://example.test/qr"))
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        Ok(LoginPoll::Authorized(AuthorizedLogin {
            cookie: "MUSIC_U=secret".into(),
        }))
    }

    fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
        Ok(AccountProfile {
            user_id: 42,
            nickname: "Singer".into(),
        })
    }
}

struct ChromeLoginProvider {
    accepted_cookie: String,
    seen_cookies: Mutex<Vec<String>>,
}

impl NeteaseProvider for ChromeLoginProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        unreachable!()
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        unreachable!()
    }

    fn account(&self, cookie: &str) -> Result<AccountProfile, NeteaseError> {
        self.seen_cookies.lock().unwrap().push(cookie.to_owned());
        if cookie != self.accepted_cookie {
            return Err(NeteaseError::LoginRequired);
        }
        Ok(AccountProfile {
            user_id: 42,
            nickname: "Singer".into(),
        })
    }
}

struct FakeChromeCookieSource {
    profiles: Vec<(ChromeCookieProfile, Vec<BrowserCookie>)>,
}

impl ChromeCookieSource for FakeChromeCookieSource {
    fn profiles(&self) -> Result<Vec<ChromeCookieProfile>, NeteaseError> {
        Ok(self
            .profiles
            .iter()
            .map(|(profile, _)| ChromeCookieProfile {
                id: profile.id.clone(),
            })
            .collect())
    }

    fn cookies(&self, profile: &ChromeCookieProfile) -> Result<Vec<BrowserCookie>, NeteaseError> {
        Ok(self
            .profiles
            .iter()
            .find(|(candidate, _)| candidate.id == profile.id)
            .map_or_else(Vec::new, |(_, cookies)| cookies.clone()))
    }
}

fn chrome_profile(
    name: &str,
    cookies: &[(&str, &str)],
) -> (ChromeCookieProfile, Vec<BrowserCookie>) {
    (
        ChromeCookieProfile { id: name.into() },
        cookies
            .iter()
            .map(|(name, value)| BrowserCookie {
                name: (*name).into(),
                value: (*value).into(),
            })
            .collect(),
    )
}

struct OfflineAccountProvider;

impl NeteaseProvider for OfflineAccountProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        unreachable!()
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        unreachable!()
    }

    fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
        Err(NeteaseError::Http("offline".into()))
    }
}

struct CatalogProvider;

impl NeteaseProvider for CatalogProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        unreachable!()
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        unreachable!()
    }

    fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
        unreachable!()
    }

    fn search(
        &self,
        _cookie: &str,
        query: &str,
        offset: usize,
        _limit: usize,
    ) -> Result<SongPage, NeteaseError> {
        assert_eq!(query, "Beyond");
        assert_eq!(offset, 0);
        Ok(SongPage {
            songs: vec![song(7, "海阔天空")],
            total: 1,
            offset,
        })
    }

    fn liked_songs(&self, _cookie: &str, user_id: u64) -> Result<Vec<Song>, NeteaseError> {
        assert_eq!(user_id, 42);
        Ok(vec![song(8, "光辉岁月")])
    }
}

fn song(id: u64, title: &str) -> Song {
    Song {
        id,
        title: title.into(),
        artists: vec!["Beyond".into()],
        album: "精选".into(),
        cover_url: Some("https://example.test/cover.jpg".into()),
        max_quality: Quality::Lossless,
        available: true,
    }
}

struct DownloadProvider {
    audio: Vec<u8>,
}

impl NeteaseProvider for DownloadProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        unreachable!()
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        unreachable!()
    }

    fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
        unreachable!()
    }

    fn audio_source(
        &self,
        _cookie: &str,
        song_id: u64,
        quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        assert_eq!(song_id, 7);
        if quality == Quality::Lossless {
            Ok(Some(AudioSource {
                url: "https://example.test/audio".into(),
                quality,
                extension: "wav".into(),
                size: self.audio.len() as u64,
            }))
        } else {
            Ok(None)
        }
    }

    fn fetch_bytes(
        &self,
        url: &str,
        cookie: Option<&str>,
        _maximum: usize,
    ) -> Result<Vec<u8>, NeteaseError> {
        assert!(
            cookie.is_none(),
            "media requests must not receive the login cookie"
        );
        match url {
            "https://example.test/audio" => Ok(self.audio.clone()),
            "https://example.test/cover.jpg" => Ok(vec![
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
                0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x08,
                0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d,
                0xb1, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
            ]),
            _ => panic!("unexpected URL: {url}"),
        }
    }
}

struct ExpiredDownloadProvider;

impl NeteaseProvider for ExpiredDownloadProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        unreachable!()
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        unreachable!()
    }

    fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
        Err(NeteaseError::LoginRequired)
    }

    fn audio_source(
        &self,
        _cookie: &str,
        _song_id: u64,
        _quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        Ok(None)
    }
}

struct FailingStreamProvider {
    attempts: Arc<std::sync::atomic::AtomicUsize>,
}

impl NeteaseProvider for FailingStreamProvider {
    fn begin_login(&self) -> Result<LoginTicket, NeteaseError> {
        unreachable!()
    }

    fn poll_login(&self, _key: &str) -> Result<LoginPoll, NeteaseError> {
        unreachable!()
    }

    fn account(&self, _cookie: &str) -> Result<AccountProfile, NeteaseError> {
        unreachable!()
    }

    fn audio_source(
        &self,
        _cookie: &str,
        _song_id: u64,
        quality: Quality,
    ) -> Result<Option<AudioSource>, NeteaseError> {
        Ok((quality == Quality::Lossless).then(|| AudioSource {
            url: "https://example.test/audio".into(),
            quality,
            extension: "wav".into(),
            size: 100,
        }))
    }

    fn fetch_audio(
        &self,
        _url: &str,
        destination: &Path,
        _maximum: usize,
        _cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<u64, NeteaseError> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        fs::write(destination, b"partial audio")?;
        Err(NeteaseError::Http("stream interrupted".into()))
    }
}

fn wav_bytes() -> Vec<u8> {
    let sandbox = tempfile::tempdir().unwrap();
    let path = sandbox.path().join("fixture.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    writer.write_sample::<i16>(0).unwrap();
    writer.finalize().unwrap();
    fs::read(path).unwrap()
}

#[test]
fn download_uses_highest_available_quality_tags_and_deduplicates() {
    use lofty::prelude::{Accessor, TaggedFileExt};

    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    store
        .save(&NeteaseSession {
            cookie: "MUSIC_U=secret".into(),
            user_id: 42,
            nickname: "Singer".into(),
        })
        .unwrap();
    let client =
        NeteaseClient::with_provider(Arc::new(DownloadProvider { audio: wav_bytes() }), store);
    let music_root = sandbox.path().join("music");
    fs::create_dir_all(&music_root).unwrap();
    let track = song(7, "海阔天空");

    let outcome = client
        .download_song(
            &music_root,
            &track,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    let DownloadOutcome::Downloaded { path, quality } = outcome else {
        panic!("expected a downloaded song")
    };
    assert_eq!(quality, Quality::Lossless);
    assert_eq!(path.file_name().unwrap(), "Beyond-海阔天空.wav");
    let tagged = lofty::read_from_path(&path).unwrap();
    let tag = tagged.primary_tag().unwrap();
    assert_eq!(tag.title().as_deref(), Some("海阔天空"));
    assert_eq!(tag.artist().as_deref(), Some("Beyond"));
    assert_eq!(tag.album().as_deref(), Some("精选"));
    assert_eq!(tag.pictures().len(), 1);
    assert_eq!(
        client.downloaded_song_ids(&music_root, &[7, 8]).unwrap(),
        [7].into_iter().collect()
    );

    assert!(matches!(
        client
            .download_song(
                &music_root,
                &track,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap(),
        DownloadOutcome::Skipped { .. }
    ));

    fs::remove_file(&path).unwrap();
    assert!(
        client
            .downloaded_song_ids(&music_root, &[7])
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        client
            .download_song(
                &music_root,
                &track,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap(),
        DownloadOutcome::Downloaded { .. }
    ));
}

#[test]
fn cancelled_download_stops_before_creating_output() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    store
        .save(&NeteaseSession {
            cookie: "MUSIC_U=secret".into(),
            user_id: 42,
            nickname: "Singer".into(),
        })
        .unwrap();
    let client =
        NeteaseClient::with_provider(Arc::new(DownloadProvider { audio: wav_bytes() }), store);
    let music_root = sandbox.path().join("music");
    fs::create_dir_all(&music_root).unwrap();
    let cancellation = std::sync::atomic::AtomicBool::new(true);

    assert!(matches!(
        client.download_song(&music_root, &song(7, "Song"), &cancellation),
        Err(NeteaseError::Cancelled)
    ));
    assert!(!music_root.join("NetEase").exists());
}

#[test]
fn download_paths_keep_both_existing_collision_candidates_intact() {
    let sandbox = tempfile::tempdir().unwrap();
    let output_root = sandbox.path();
    let base = output_root.join("Beyond-Song.wav");
    let with_id = output_root.join("Beyond-Song[7].wav");
    fs::write(&base, b"base file").unwrap();
    fs::write(&with_id, b"id file").unwrap();
    let source = AudioSource {
        url: "https://example.test/audio".into(),
        quality: Quality::Lossless,
        extension: "wav".into(),
        size: 1,
    };

    let paths = DownloadPaths::new(
        output_root,
        &song(7, "Song"),
        &source,
        &DownloadDecision::Download,
    );

    assert_eq!(
        paths.destination.file_name().unwrap(),
        "Beyond-Song[7]-2.wav"
    );
    assert_eq!(fs::read(base).unwrap(), b"base file");
    assert_eq!(fs::read(with_id).unwrap(), b"id file");
    assert!(!paths.destination.exists());
}

#[test]
fn failed_stream_retries_and_removes_the_partial_audio_file() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    store
        .save(&NeteaseSession {
            cookie: "MUSIC_U=secret".into(),
            user_id: 42,
            nickname: "Singer".into(),
        })
        .unwrap();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let client = NeteaseClient::with_provider(
        Arc::new(FailingStreamProvider {
            attempts: Arc::clone(&attempts),
        }),
        store,
    );
    let music_root = sandbox.path().join("music");

    assert!(matches!(
        client.download_song(
            &music_root,
            &song(7, "Song"),
            &std::sync::atomic::AtomicBool::new(false),
        ),
        Err(NeteaseError::Http(message)) if message == "stream interrupted"
    ));
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 4);
    let output_root = music_root.join("NetEase");
    assert!(fs::read_dir(output_root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".part.")
    }));
}

#[test]
fn unavailable_audio_revalidates_the_session_before_reporting_rights_failure() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    store
        .save(&NeteaseSession {
            cookie: "expired-cookie".into(),
            user_id: 42,
            nickname: "Singer".into(),
        })
        .unwrap();
    let client = NeteaseClient::with_provider(Arc::new(ExpiredDownloadProvider), store);

    assert!(matches!(
        client.download_song(
            sandbox.path(),
            &song(7, "Song"),
            &std::sync::atomic::AtomicBool::new(false),
        ),
        Err(NeteaseError::LoginRequired)
    ));
}

#[test]
fn logged_in_user_can_search_tracks_and_load_liked_songs() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    store
        .save(&NeteaseSession {
            cookie: "MUSIC_U=secret".into(),
            user_id: 42,
            nickname: "Singer".into(),
        })
        .unwrap();
    let client = NeteaseClient::with_provider(Arc::new(CatalogProvider), store);

    let results = client.search("Beyond", 0, 50).unwrap();
    assert_eq!(results.total, 1);
    assert_eq!(results.songs[0].title, "海阔天空");
    assert_eq!(client.liked_songs().unwrap()[0].title, "光辉岁月");
}

#[test]
fn catalog_requires_a_saved_login() {
    let sandbox = tempfile::tempdir().unwrap();
    let client = NeteaseClient::with_provider(
        Arc::new(CatalogProvider),
        SessionStore::at(sandbox.path().join("missing.json")),
    );

    assert!(matches!(
        client.search("Beyond", 0, 50),
        Err(NeteaseError::LoginRequired)
    ));
}

#[test]
fn authorized_qr_login_persists_a_redacted_session() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    let client = NeteaseClient::with_provider(Arc::new(LoginProvider), store.clone());

    let ticket = client.begin_login().unwrap();
    assert_eq!(ticket.qr_url(), "https://example.test/qr");
    assert_eq!(client.poll_login(&ticket).unwrap(), LoginStatus::LoggedIn);
    assert_eq!(client.session().unwrap().unwrap().nickname, "Singer");
    assert!(!format!("{:?}", client.session().unwrap().unwrap()).contains("secret"));
    assert!(store.path().is_file());
}

#[test]
fn chrome_login_filters_credentials_validates_profiles_and_persists_the_first_valid_one() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    let provider = Arc::new(ChromeLoginProvider {
        accepted_cookie: "MUSIC_U=valid; __csrf=token".into(),
        seen_cookies: Mutex::new(Vec::new()),
    });
    let client = NeteaseClient::with_provider(provider.clone(), store.clone());
    let source = FakeChromeCookieSource {
        profiles: vec![
            chrome_profile("Profile 1", &[("MUSIC_U", "expired")]),
            chrome_profile(
                "Default",
                &[
                    ("tracking_cookie", "must-not-leave-browser"),
                    ("__csrf", "token"),
                    ("MUSIC_U", "valid"),
                ],
            ),
        ],
    };

    let session = client.import_chrome_session_from(&source).unwrap();

    assert_eq!(session.nickname(), "Singer");
    assert_eq!(
        provider.seen_cookies.lock().unwrap().as_slice(),
        ["MUSIC_U=expired", "MUSIC_U=valid; __csrf=token"]
    );
    assert_eq!(store.load().unwrap(), Some(session));
}

#[test]
fn chrome_login_rejects_unsafe_cookie_values_without_contacting_netease() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    let provider = Arc::new(ChromeLoginProvider {
        accepted_cookie: "never".into(),
        seen_cookies: Mutex::new(Vec::new()),
    });
    let client = NeteaseClient::with_provider(provider.clone(), store.clone());
    let source = FakeChromeCookieSource {
        profiles: vec![chrome_profile(
            "Default",
            &[("MUSIC_U", "secret\r\nInjected: value")],
        )],
    };

    assert!(matches!(
        client.import_chrome_session_from(&source),
        Err(NeteaseError::ChromeLogin(_))
    ));
    assert!(provider.seen_cookies.lock().unwrap().is_empty());
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn chrome_login_preserves_account_service_failures_instead_of_calling_them_rejections() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    let client = NeteaseClient::with_provider(Arc::new(OfflineAccountProvider), store.clone());
    let source = FakeChromeCookieSource {
        profiles: vec![chrome_profile("Default", &[("MUSIC_U", "valid")])],
    };

    assert!(matches!(
        client.import_chrome_session_from(&source),
        Err(NeteaseError::Http(message)) if message == "offline"
    ));
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn session_round_trips_and_logout_removes_the_credential() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = SessionStore::at(sandbox.path().join("session.json"));
    let session = NeteaseSession {
        cookie: "MUSIC_U=secret; __csrf=token".into(),
        user_id: 42,
        nickname: "Singer".into(),
    };

    store.save(&session).unwrap();
    assert_eq!(store.load().unwrap(), Some(session));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    store.clear().unwrap();
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn risk_acceptance_is_persisted_separately_from_the_session() {
    let sandbox = tempfile::tempdir().unwrap();
    let risk = super::RiskStore::at(sandbox.path().join("preferences.json"));
    let session = SessionStore::at(sandbox.path().join("session.json"));

    assert!(!risk.accepted().unwrap());
    risk.accept().unwrap();
    assert!(risk.accepted().unwrap());
    assert_eq!(session.load().unwrap(), None);
}

#[test]
fn retries_transient_failures_but_not_deterministic_protocol_errors() {
    let mut transient_attempts = 0;
    let value = super::retry_transient(|| {
        transient_attempts += 1;
        if transient_attempts < 3 {
            Err(NeteaseError::Http("temporary failure".into()))
        } else {
            Ok("recovered")
        }
    })
    .unwrap();
    assert_eq!(value, "recovered");
    assert_eq!(transient_attempts, 3);

    let mut deterministic_attempts = 0;
    assert!(matches!(
        super::retry_transient::<()>(|| {
            deterministic_attempts += 1;
            Err(NeteaseError::Protocol("not available".into()))
        }),
        Err(NeteaseError::Protocol(_))
    ));
    assert_eq!(deterministic_attempts, 1);
}

#[test]
fn web_provider_uses_the_download_endpoint_with_the_requested_quality_bitrate() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4_096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client closed before sending the request body");
            request.extend_from_slice(&buffer[..count]);
            let Some(header_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or_default();
            if request.len() >= header_end + content_length {
                break;
            }
        }
        let body = r#"{"code":200,"data":{"code":200,"url":"https://cdn.example.test/song.flac","type":"flac","size":22961285,"level":"lossless"}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    let provider = super::WebNeteaseProvider::at_base_url(base_url);

    let source = provider
        .audio_source("MUSIC_U=secret", 185_726, Quality::Lossless)
        .unwrap()
        .unwrap();
    let request = server.join().unwrap();

    assert!(request.starts_with("POST /weapi/song/enhance/download/url HTTP/1.1"));
    assert!(request.contains("content-type: application/x-www-form-urlencoded"));
    assert!(request.contains("params="));
    assert!(request.contains("encSecKey="));
    assert!(!request.contains("id=185726"));
    assert!(!request.contains("br=999000"));
    assert!(!request.contains("player/url"));
    assert_eq!(source.url, "https://cdn.example.test/song.flac");
    assert_eq!(source.quality, Quality::Lossless);
    assert_eq!(source.extension, "flac");
    assert_eq!(source.size, 22_961_285);
}

#[test]
fn weapi_cookie_context_preserves_values_from_the_saved_session() {
    let header = super::weapi_cookie_header("MUSIC_U=secret; __csrf=token; os=pc; appver=9.9.9");

    assert!(header.contains("MUSIC_U=secret"));
    assert!(header.contains("__csrf=token"));
    assert!(header.contains("os=pc"));
    assert!(header.contains("appver=9.9.9"));
    assert!(!header.contains("os=osx"));
    assert!(!header.contains("appver=3.0.12"));
    assert!(header.contains("channel=appstore"));
    assert!(header.contains("__remember_me=true"));
}

#[test]
#[ignore = "contacts the live undocumented NetEase endpoint"]
fn live_weapi_download_url_returns_fetchable_media() {
    let session_path = std::env::var_os("K3_NETEASE_SESSION_PATH")
        .expect("K3_NETEASE_SESSION_PATH must point to a saved session");
    let session = SessionStore::at(session_path.into())
        .load()
        .unwrap()
        .expect("the saved session must exist");
    let provider = super::WebNeteaseProvider::new();

    let source = provider
        .audio_source(&session.cookie, 301_448, Quality::Lossless)
        .unwrap()
        .expect("翩翩飞起 should have a lossless source");
    let response = provider
        .agent
        .get(&source.url)
        .header("User-Agent", super::NETEASE_USER_AGENT)
        .header("Referer", super::NETEASE_BASE_URL)
        .call()
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("Content-Length")
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap(),
        source.size
    );
}

#[test]
#[ignore = "contacts the live undocumented NetEase endpoint"]
fn live_endpoint_can_issue_a_qr_login_ticket() {
    let provider = super::WebNeteaseProvider::new();
    let ticket = provider.begin_login().unwrap();

    assert_ne!(ticket.qr_lines().unwrap(), Vec::<String>::new());
}

#[test]
fn download_index_skips_equal_quality_and_replaces_lower_quality() {
    let sandbox = tempfile::tempdir().unwrap();
    let path = sandbox.path().join("index.json");
    let mut index = DownloadIndex::load(&path).unwrap();
    assert_eq!(
        index.decision(7, Quality::Lossless),
        DownloadDecision::Download
    );

    index.record(DownloadRecord {
        song_id: 7,
        quality: Quality::Lossless,
        path: "Artist-Song.flac".into(),
    });
    fs::write(sandbox.path().join("Artist-Song.flac"), b"audio").unwrap();
    index.save().unwrap();

    let loaded = DownloadIndex::load(&path).unwrap();
    assert_eq!(
        loaded.cached_path(7),
        Some(sandbox.path().join("Artist-Song.flac"))
    );
    assert_eq!(loaded.decision(7, Quality::ExHigh), DownloadDecision::Skip);
    assert_eq!(
        loaded.decision(7, Quality::Lossless),
        DownloadDecision::Skip
    );
    assert_eq!(
        loaded.decision(7, Quality::HiRes),
        DownloadDecision::Upgrade("Artist-Song.flac".into())
    );

    let mut tampered = DownloadIndex::load(&sandbox.path().join("tampered.json")).unwrap();
    tampered.record(DownloadRecord {
        song_id: 8,
        quality: Quality::Lossless,
        path: "../outside.flac".into(),
    });
    assert_eq!(
        tampered.decision(8, Quality::HiRes),
        DownloadDecision::Download
    );
    assert_eq!(tampered.cached_path(8), None);
}

#[test]
fn download_filename_is_flat_portable_and_only_uses_primary_artist() {
    assert_eq!(
        download_filename("Artist", "Song: Live?", "flac", 123),
        "Artist-Song_ Live_.flac"
    );
    assert_eq!(download_filename("A/B", "Same", "mp3", 456), "A_B-Same.mp3");
    assert_eq!(
        download_filename("Artist", "Song", "../../FLAC", 789),
        "Artist-Song.flac"
    );
}
