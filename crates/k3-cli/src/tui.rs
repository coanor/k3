use std::{
    collections::VecDeque,
    error::Error,
    fs,
    io::{self, stdout},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use k3_app::{
    AudioRecorder, LyricsChoice, LyricsProgress, LyricsSearch, PlaybackCommand, PlaybackStatus,
    RecordingTimelineAnchor, SessionPlayback, SessionTrackKind, TrackKind as ProjectTrackKind,
    default_lyrics_query, find_lyrics_again, find_missing_lyrics, lyric_countdown, lyric_window,
    place_recording_on_timeline, render_take_mix, render_take_preview, save_lyrics_choice,
};
use k3_core::{
    FileProjectRepository, LyricsTimeline, Project, ProjectPath, ProjectRepository,
    RecordingSession, RecordingState, Take, VocalEffectPreset,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::CellWidth,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    library::{self, LibraryConfig, LibrarySnapshot, SourceEntry, SourceProjectState},
    netease::local_stores,
};

mod netease;

use self::netease::{
    MusicSource, NeteaseNotice, NeteasePanel, draw_netease_modal, draw_netease_sources,
    handle_netease_modal_key, handle_netease_source_key, poll_netease, toggle_music_source,
};

#[cfg(test)]
use self::netease::{
    CatalogJob, CatalogResult, ChromeLoginJob, ManagedTask, NeteaseDownloadJob, NeteaseModal,
    NeteaseView, netease_session_expired_hint, netease_sign_in_hint, netease_song_label,
    poll_netease_catalog, poll_netease_chrome_login, poll_netease_download,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryFocus {
    Projects,
    Project,
    Sources,
}

impl LibraryFocus {
    const fn next(self) -> Self {
        match self {
            Self::Projects => Self::Project,
            Self::Project => Self::Sources,
            Self::Sources => Self::Projects,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Projects => Self::Sources,
            Self::Project => Self::Projects,
            Self::Sources => Self::Project,
        }
    }
}

struct ImportJob {
    source: PathBuf,
    result: Receiver<Result<PathBuf, String>>,
    cancellation: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct SeparationRequest {
    source: SourceEntry,
}

struct MediaLibrary {
    config: LibraryConfig,
    snapshot: LibrarySnapshot,
    focus: LibraryFocus,
    project_selected: usize,
    source_selected: usize,
    job: Option<ImportJob>,
    queue: VecDeque<SeparationRequest>,
    message: Option<String>,
    netease_message: Option<NeteaseNotice>,
    music_source: MusicSource,
    netease: Option<NeteasePanel>,
    confirm_exit: bool,
}

impl MediaLibrary {
    fn new(config: LibraryConfig) -> Result<Self, Box<dyn Error>> {
        let snapshot = library::scan(&config)?;
        let netease = if config.netease.enabled {
            let (session_store, risk_store) = local_stores()?;
            Some(NeteasePanel::new(session_store, risk_store)?)
        } else {
            None
        };
        Ok(Self {
            config,
            snapshot,
            focus: LibraryFocus::Projects,
            project_selected: 0,
            source_selected: 0,
            job: None,
            queue: VecDeque::new(),
            message: None,
            netease_message: None,
            music_source: MusicSource::Local,
            netease,
            confirm_exit: false,
        })
    }

    fn refresh(&mut self) -> Result<(), Box<dyn Error>> {
        self.snapshot = library::scan(&self.config)?;
        self.project_selected = self
            .project_selected
            .min(self.snapshot.projects.len().saturating_sub(1));
        self.source_selected = self
            .source_selected
            .min(self.snapshot.sources.len().saturating_sub(1));
        Ok(())
    }

    fn refresh_after_netease_download(&mut self) {
        if let Err(error) = self.refresh() {
            let warning = format!("Library refresh failed: {error}");
            if let Some(message) = &mut self.netease_message {
                message.append_error(warning);
            } else {
                self.netease_message = Some(NeteaseNotice::error(warning));
            }
        }
    }

    fn start_import(&mut self) {
        let Some(source) = self.snapshot.sources.get(self.source_selected) else {
            self.message = Some("No importable audio files".into());
            return;
        };
        let request = SeparationRequest {
            source: source.clone(),
        };
        let path = request.source.path.clone();
        if self.job.as_ref().is_some_and(|job| job.source == path) {
            self.message = Some(format!("Separating: {}", display_name(&path)));
            return;
        }
        if let Some(position) = self
            .queue
            .iter()
            .position(|queued| queued.source.path == path)
        {
            self.message = Some(format!(
                "Already queued at position {}: {}",
                position + 1,
                display_name(&path)
            ));
            return;
        }
        if self.job.is_some() {
            self.queue.push_back(request);
            self.message = Some(format!(
                "Queued at position {}: {}",
                self.queue.len(),
                display_name(&path)
            ));
            return;
        }
        let project_state = request.source.project_state;
        self.launch_import(request);
        self.message = Some(match project_state {
            SourceProjectState::New => {
                format!("Creating project and separating: {}", display_name(&path))
            }
            SourceProjectState::Current => {
                format!("Re-separating and replacing stems: {}", display_name(&path))
            }
            SourceProjectState::ReplaceSource => {
                format!("Updating project source and stems: {}", display_name(&path))
            }
        });
    }

    fn launch_import(&mut self, request: SeparationRequest) {
        let worker_source = request.source.path.clone();
        let worker_project = request.source.project_path.clone();
        let project_state = request.source.project_state;
        let config = self.config.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = cancellation.clone();
        let (sender, result) = mpsc::channel();
        let worker = thread::spawn(move || {
            let outcome = match project_state {
                SourceProjectState::New => library::import_and_separate_with_cancellation(
                    &config,
                    &worker_source,
                    worker_cancellation,
                ),
                SourceProjectState::Current => library::reseparate_with_cancellation(
                    &config,
                    &worker_source,
                    &worker_project,
                    worker_cancellation,
                ),
                SourceProjectState::ReplaceSource => {
                    library::replace_source_and_reseparate_with_cancellation(
                        &config,
                        &worker_source,
                        &worker_project,
                        worker_cancellation,
                    )
                }
            }
            .map_err(|error| error.to_string());
            let _ = sender.send(outcome);
        });
        self.job = Some(ImportJob {
            source: request.source.path,
            result,
            cancellation,
            worker: Some(worker),
        });
    }

    fn cancel_imports(&mut self) {
        self.queue.clear();
        if let Some(mut job) = self.job.take() {
            job.cancellation.store(true, Ordering::Release);
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn start_next_queued(&mut self) -> Option<PathBuf> {
        let request = self.queue.pop_front()?;
        let path = request.source.path.clone();
        self.launch_import(request);
        Some(path)
    }

    fn enqueue_downloaded_source(&mut self, path: &Path) -> bool {
        let source = library::source_entry_for_path(&self.config, path);
        if source.project_state == SourceProjectState::Current
            || self.job.as_ref().is_some_and(|job| job.source == path)
            || self.queue.iter().any(|request| request.source.path == path)
        {
            return false;
        }
        self.queue.push_back(SeparationRequest { source });
        true
    }

    fn project_is_being_separated(&self, project_path: &Path) -> bool {
        self.job.as_ref().is_some_and(|job| {
            library::source_entry_for_path(&self.config, &job.source).project_path == project_path
        })
    }
}

impl Drop for MediaLibrary {
    fn drop(&mut self) {
        self.cancel_imports();
    }
}

fn playback_command_for_key(key: KeyCode) -> Option<PlaybackCommand> {
    match key {
        KeyCode::Char(' ') => Some(PlaybackCommand::Toggle),
        KeyCode::Left => Some(PlaybackCommand::SeekBy(-5)),
        KeyCode::Right => Some(PlaybackCommand::SeekBy(5)),
        KeyCode::Char('r') => Some(PlaybackCommand::Restart),
        KeyCode::Char('1') => Some(PlaybackCommand::SwitchTrack(ProjectTrackKind::Original)),
        KeyCode::Char('2') => Some(PlaybackCommand::SwitchTrack(
            ProjectTrackKind::Accompaniment,
        )),
        KeyCode::Char('3') => Some(PlaybackCommand::SwitchTrack(ProjectTrackKind::Vocals)),
        _ => None,
    }
}

fn recording_track_for_key(key: KeyCode) -> Option<SessionTrackKind> {
    match key {
        KeyCode::Char('1') => Some(SessionTrackKind::Original),
        KeyCode::Char('2') => Some(SessionTrackKind::Accompaniment),
        KeyCode::Char('3') => Some(SessionTrackKind::Vocals),
        _ => None,
    }
}

fn handle_playback_key(playback: &mut SessionPlayback, key: KeyCode) -> bool {
    if key == KeyCode::Char('q') {
        return true;
    }
    if let Some(command) = playback_command_for_key(key) {
        playback.execute(command);
    } else {
        match key {
            KeyCode::Char('-') => playback.adjust_volume(-0.1),
            KeyCode::Char('+' | '=') => playback.adjust_volume(0.1),
            _ => {}
        }
    }
    false
}

struct ActiveRecording {
    recorder: AudioRecorder,
    id: String,
    dry_relative_path: String,
    dry_temporary_path: PathBuf,
    dry_final_path: PathBuf,
    mix_relative_path: String,
    mix_temporary_path: PathBuf,
    mix_final_path: PathBuf,
    backing_path: PathBuf,
    backing_key_shift_semitones: i8,
    timeline: Vec<RecordingTimelineAnchor>,
}

struct LyricsPicker {
    choices: Vec<LyricsChoice>,
    selected: usize,
}

struct LyricsQueryEditor {
    query: String,
    cursor: usize,
}

impl LyricsQueryEditor {
    fn new(query: String) -> Self {
        let cursor = query.chars().count();
        Self { query, cursor }
    }

    fn insert(&mut self, character: char) {
        let byte = char_index_to_byte(&self.query, self.cursor);
        self.query.insert(byte, character);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = char_index_to_byte(&self.query, self.cursor - 1);
        let end = char_index_to_byte(&self.query, self.cursor);
        self.query.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.query.chars().count() {
            return;
        }
        let start = char_index_to_byte(&self.query, self.cursor);
        let end = char_index_to_byte(&self.query, self.cursor + 1);
        self.query.replace_range(start..end, "");
    }
}

struct LyricsSearchJob {
    result: Receiver<Result<LyricsSearch, String>>,
}

#[derive(Clone, Copy)]
enum LyricsSources {
    Lrclib,
    LrclibWithNetease,
}

impl LyricsSources {
    const fn netease_fallback(self) -> bool {
        matches!(self, Self::LrclibWithNetease)
    }
}

struct App {
    playback: SessionPlayback,
    session: RecordingSession,
    active_recording: Option<ActiveRecording>,
    recording_message: Option<String>,
    project_dirty: bool,
    monitoring_enabled: bool,
    selected_take: Option<usize>,
    effect_selecting: bool,
    pending_take_delete: Option<String>,
    default_effect: VocalEffectPreset,
    lyrics: Option<LyricsTimeline>,
    lyrics_origin: Option<String>,
    lyrics_picker: Option<LyricsPicker>,
    lyrics_query_editor: Option<LyricsQueryEditor>,
    lyrics_search_job: Option<LyricsSearchJob>,
    lyrics_sources: LyricsSources,
}

impl App {
    fn new(
        project: Project,
        lyrics: Option<LyricsTimeline>,
        startup_message: Option<String>,
        default_effect: VocalEffectPreset,
    ) -> Self {
        let selected_take = project.takes().len().checked_sub(1);
        let playback = SessionPlayback::open(&project);
        Self {
            playback,
            session: RecordingSession::new(project),
            active_recording: None,
            recording_message: startup_message,
            project_dirty: false,
            monitoring_enabled: false,
            selected_take,
            effect_selecting: false,
            pending_take_delete: None,
            default_effect,
            lyrics,
            lyrics_origin: None,
            lyrics_picker: None,
            lyrics_query_editor: None,
            lyrics_search_job: None,
            lyrics_sources: LyricsSources::Lrclib,
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
        if self.pending_take_delete.is_some() {
            if matches!(key, KeyCode::Char('y' | 'Y')) {
                self.delete_selected_take();
            } else if matches!(
                key,
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q')
            ) {
                self.pending_take_delete = None;
                self.recording_message = Some("Take kept".into());
            }
            return false;
        }
        if self.lyrics_query_editor.is_some() {
            self.handle_lyrics_query_key(key);
            return false;
        }
        if self.lyrics_picker.is_some() {
            self.handle_lyrics_picker_key(key);
            return false;
        }
        if self.lyrics_search_job.is_some() && key == KeyCode::Char('a') {
            self.recording_message = Some("Wait for the lyrics search before recording".into());
            return false;
        }
        if self.effect_selecting {
            if let Some(preset) = effect_preset_for_key(key) {
                self.effect_selecting = false;
                self.apply_take_effect(preset);
                return false;
            }
            if matches!(key, KeyCode::Esc | KeyCode::Char('e')) {
                self.effect_selecting = false;
                self.recording_message = Some("Effect selection cancelled".into());
                return false;
            }
            self.effect_selecting = false;
        }
        match (self.session.state(), key) {
            (_, KeyCode::Char('m')) => self.toggle_monitoring(),
            (RecordingState::Idle, KeyCode::Char('a')) => self.arm(),
            (RecordingState::Idle, KeyCode::Char('[')) => self.select_take(-1),
            (RecordingState::Idle, KeyCode::Char(']')) => self.select_take(1),
            (RecordingState::Idle, KeyCode::Char('e')) => self.begin_effect_selection(),
            (RecordingState::Idle, KeyCode::Char('4')) => self.play_selected_take(),
            (RecordingState::Idle, KeyCode::Delete) => {
                self.pending_take_delete = self
                    .selected_take
                    .map(|index| self.session.project().takes()[index].id().to_owned());
                if self.pending_take_delete.is_none() {
                    self.recording_message = Some("Current project has no takes".into());
                }
            }
            (RecordingState::Idle, KeyCode::Char(',')) => self.adjust_key(-1),
            (RecordingState::Idle, KeyCode::Char('.')) => self.adjust_key(1),
            (RecordingState::Idle, KeyCode::Char('/')) => self.reset_key(),
            (RecordingState::Idle, KeyCode::Char('l')) => self.begin_lyrics_search(),
            (RecordingState::Armed, KeyCode::Enter) => self.start_recording(),
            (RecordingState::Armed, KeyCode::Esc) => self.cancel_arm(),
            (RecordingState::Recording, KeyCode::Enter) => {
                self.finish_recording();
            }
            (RecordingState::Recording, KeyCode::Char('q')) => {
                return self.finish_recording();
            }
            (RecordingState::Recording, KeyCode::Char('-' | '+' | '=')) => {
                handle_playback_key(&mut self.playback, key);
            }
            (RecordingState::Recording, KeyCode::Left) => {
                self.seek_recording_by_lyric(-1);
            }
            (RecordingState::Recording, KeyCode::Right) => {
                self.seek_recording_by_lyric(1);
            }
            (RecordingState::Recording, key) if let Some(kind) = recording_track_for_key(key) => {
                self.playback.switch_track(kind);
                self.anchor_recording_at_playback_position();
                self.recording_message = self.playback.error().map_or_else(
                    || {
                        Some(format!(
                            "Recording continues · monitoring {} · take still mixes accompaniment only",
                            self.playback.selected_track().label()
                        ))
                    },
                    |error| Some(format!("Failed to switch monitor track: {error}")),
                );
            }
            (RecordingState::Recording, _) => {
                self.recording_message =
                    Some("Recording: 1/2/3 switch monitor track · Enter stops".into());
            }
            (RecordingState::Armed, KeyCode::Char('q')) => {
                self.cancel_arm();
                return true;
            }
            (_, KeyCode::Char('q')) => {
                return self.save_project();
            }
            _ => return handle_playback_key(&mut self.playback, key),
        }
        false
    }

    fn handle_lyrics_picker_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Up => {
                if let Some(picker) = &mut self.lyrics_picker {
                    picker.selected = picker.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(picker) = &mut self.lyrics_picker {
                    picker.selected = (picker.selected + 1).min(picker.choices.len() - 1);
                }
            }
            KeyCode::Esc => {
                self.lyrics_picker = None;
                self.recording_message = Some("Lyrics download skipped".into());
            }
            KeyCode::Enter => self.save_selected_lyrics(),
            _ => {}
        }
    }

    fn save_selected_lyrics(&mut self) {
        let Some(mut picker) = self.lyrics_picker.take() else {
            return;
        };
        let choice = picker.choices.remove(picker.selected);
        match save_lyrics_choice(self.session.project(), choice, &mut |_| {}) {
            Ok((saved, relative_path)) => {
                if let Err(error) = self.session.set_lyrics(relative_path) {
                    self.recording_message = Some(format!("Cannot attach lyrics: {error}"));
                    return;
                }
                self.project_dirty = true;
                if !self.save_project() {
                    return;
                }
                match load_lyrics(self.session.project()) {
                    Ok(lyrics) => {
                        self.lyrics = lyrics;
                        self.lyrics_origin = Some(saved.origin.clone());
                        self.recording_message = Some(format!(
                            "Downloaded lyrics: {} - {} · {}",
                            saved.artist, saved.track, saved.origin
                        ));
                    }
                    Err(error) => {
                        self.recording_message = Some(format!("Cannot load lyrics: {error}"));
                    }
                }
            }
            Err(error) => {
                self.recording_message = Some(format!("Lyrics download failed: {error}"));
            }
        }
    }

    fn begin_lyrics_search(&mut self) {
        if self.lyrics_search_job.is_some() {
            self.recording_message = Some("Lyrics search is already running".into());
            return;
        }
        let query = default_lyrics_query(self.session.project());
        self.lyrics_query_editor = Some(LyricsQueryEditor::new(query));
        self.recording_message = Some("Edit the lyrics search query, then press Enter".into());
    }

    fn handle_lyrics_query_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                self.lyrics_query_editor = None;
                self.recording_message = Some("Lyrics search cancelled".into());
            }
            KeyCode::Enter => {
                let query = self
                    .lyrics_query_editor
                    .as_ref()
                    .map(|editor| editor.query.trim().to_owned())
                    .unwrap_or_default();
                if query.is_empty() {
                    self.recording_message = Some("Lyrics search query cannot be empty".into());
                } else {
                    self.lyrics_query_editor = None;
                    self.start_lyrics_search(query);
                }
            }
            KeyCode::Left => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.cursor = editor.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.cursor = (editor.cursor + 1).min(editor.query.chars().count());
                }
            }
            KeyCode::Home => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.cursor = editor.query.chars().count();
                }
            }
            KeyCode::Backspace => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.delete();
                }
            }
            KeyCode::Char(character) if !character.is_control() => {
                if let Some(editor) = &mut self.lyrics_query_editor {
                    editor.insert(character);
                }
            }
            _ => {}
        }
    }

    fn start_lyrics_search(&mut self, query: String) {
        let project = self.session.project().clone();
        let netease_fallback = self.lyrics_sources.netease_fallback();
        let status_query = query.clone();
        let (sender, result) = mpsc::channel();
        thread::spawn(move || {
            let outcome = find_lyrics_again(&project, &query, netease_fallback, &mut |_| {})
                .map_err(|error| error.to_string());
            let _ = sender.send(outcome);
        });
        self.lyrics_search_job = Some(LyricsSearchJob { result });
        self.recording_message = Some(format!("Searching online lyrics: {status_query}"));
    }

    fn poll_lyrics_search(&mut self) {
        let outcome = match self
            .lyrics_search_job
            .as_ref()
            .map(|job| job.result.try_recv())
        {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Err("Lyrics search task exited unexpectedly".into()))
            }
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        let Some(outcome) = outcome else {
            return;
        };
        self.lyrics_search_job = None;
        match outcome {
            Ok(LyricsSearch::Candidates(choices)) => {
                let count = choices.len();
                self.lyrics_picker = Some(LyricsPicker {
                    choices,
                    selected: 0,
                });
                self.recording_message = Some(format!(
                    "Found {count} lyric matches · choose one before downloading"
                ));
            }
            Ok(LyricsSearch::NotFound) => {
                self.recording_message =
                    Some("No duration-matched synced lyrics found · current lyrics kept".into());
            }
            Ok(LyricsSearch::AlreadyPresent) => {
                self.recording_message = Some("Current lyrics are already available".into());
            }
            Err(error) => {
                self.recording_message = Some(format!(
                    "Lyrics search failed: {error} · current lyrics kept"
                ));
            }
        }
    }

    fn toggle_monitoring(&mut self) {
        self.monitoring_enabled = !self.monitoring_enabled;
        if let Some(active) = &self.active_recording {
            active.recorder.set_monitoring(self.monitoring_enabled);
        }
        self.recording_message = Some(if self.monitoring_enabled {
            "Microphone monitor enabled · use headphones to avoid feedback".into()
        } else {
            "Microphone monitor disabled".into()
        });
    }

    fn seek_recording_by_lyric(&mut self, direction: i8) {
        let Some(timeline) = self.lyrics.as_ref() else {
            self.recording_message = Some("No synced lyrics; lyric seek is unavailable".into());
            return;
        };
        let Some(target) = lyric_seek_target(timeline, self.playback.position(), direction) else {
            self.recording_message = Some("Reached the lyric timeline boundary".into());
            return;
        };
        if !self.playback.has_audio() {
            self.recording_message = Some("playback is unavailable; cannot seek recording".into());
            return;
        }
        self.playback.execute(PlaybackCommand::SeekTo(target));
        if let Some(error) = self.playback.error() {
            self.recording_message = Some(format!("Lyric seek failed: {error}"));
            return;
        }
        self.anchor_recording(target);
        self.recording_message = Some(format!(
            "Recording continues · jumped to {} · rerecording overwrites that range",
            format_duration(target)
        ));
    }

    fn anchor_recording_at_playback_position(&mut self) {
        self.anchor_recording(self.playback.position());
    }

    fn anchor_recording(&mut self, song_position: Duration) {
        if let Some(active) = &mut self.active_recording {
            active.timeline.push(RecordingTimelineAnchor {
                capture_frame: active.recorder.captured_frames(),
                song_position,
            });
        }
    }

    fn select_take(&mut self, direction: i32) {
        let count = self.session.project().takes().len();
        let Some(current) = self.selected_take else {
            self.recording_message = Some("Current project has no takes".into());
            return;
        };
        let next = if direction.is_negative() {
            current.checked_sub(1).unwrap_or(count - 1)
        } else {
            (current + 1) % count
        };
        self.selected_take = Some(next);
        let take = &self.session.project().takes()[next];
        self.recording_message = Some(format!(
            "selected take {}/{}: {} · effect {}",
            next + 1,
            count,
            take.id(),
            take.effect_preset().label()
        ));
    }

    fn play_selected_take(&mut self) {
        let Some(index) = self.selected_take else {
            self.recording_message = Some("Current project has no takes".into());
            return;
        };
        let take = &self.session.project().takes()[index];
        let stale = take.mix_audio().is_none()
            || take.rendered_key_semitones() != self.session.project().key_shift_semitones();
        let path = if stale {
            match self.rerender_take(index, take.effect_preset()) {
                Ok(path) => path,
                Err(error) => {
                    self.recording_message = Some(format!("Failed to rebuild take mix: {error}"));
                    return;
                }
            }
        } else {
            self.session
                .project()
                .root()
                .join(Path::new(take.mix_audio().expect("checked above").as_str()))
        };
        let result = self.playback.play_take(&path);
        self.recording_message = result.err().map_or_else(
            || Some("playing selected take mix".into()),
            |error| Some(error.to_string()),
        );
    }

    fn delete_selected_take(&mut self) {
        let Some(take_id) = self.pending_take_delete.take() else {
            return;
        };
        if !self.save_project() {
            return;
        }
        let previous_index = self.selected_take.unwrap_or(0);
        let unloaded = self.playback.execute(PlaybackCommand::Unload);
        if unloaded.status == PlaybackStatus::Error {
            self.recording_message = Some("Cannot release audio before deleting the take".into());
            return;
        }
        let result = FileProjectRepository.delete_take(
            self.session.project().root(),
            &take_id,
            self.session.project().document_revision(),
        );
        match result {
            Ok(deleted) => {
                self.selected_take = (!deleted.project.takes().is_empty())
                    .then(|| previous_index.min(deleted.project.takes().len() - 1));
                self.session = RecordingSession::new(deleted.project);
                self.project_dirty = false;
                self.recording_message = Some(format!(
                    "Deleted take {take_id}{}",
                    deleted
                        .cleanup_warning
                        .map_or_else(String::new, |warning| format!(" · warning: {warning}"))
                ));
            }
            Err(error) => {
                self.recording_message = Some(format!("Cannot delete take: {error}"));
            }
        }
        let loaded = self
            .selected_take
            .and_then(|index| {
                k3_app::LoadedProject::from_project_with_lyrics_for_take(
                    self.session.project(),
                    self.session.project().takes()[index].id(),
                )
                .ok()
            })
            .unwrap_or_else(|| k3_app::LoadedProject::from_project(self.session.project()));
        self.playback.execute(PlaybackCommand::Load(loaded));
    }

    fn begin_effect_selection(&mut self) {
        if self.selected_take.is_none() {
            self.recording_message = Some("Current project has no takes".into());
            return;
        }
        self.effect_selecting = true;
        self.recording_message = Some(
            "Select effect: 1 clean · 2 studio · 3 ktv · 4 theater · 5 church · Esc cancel".into(),
        );
    }

    fn apply_take_effect(&mut self, preset: VocalEffectPreset) {
        let Some(index) = self.selected_take else {
            self.recording_message = Some("Current project has no takes".into());
            return;
        };
        match self.rerender_take(index, preset) {
            Ok(path) => {
                let playback_warning = self.playback.play_take(&path).err();
                self.recording_message = Some(playback_warning.map_or_else(
                    || format!("take effect: {} · mix rebuilt and playing", preset.label()),
                    |error| format!("take effect: {} · playback error: {error}", preset.label()),
                ));
            }
            Err(error) => self.recording_message = Some(format!("effect render failed: {error}")),
        }
    }

    fn rerender_take(
        &mut self,
        index: usize,
        preset: VocalEffectPreset,
    ) -> Result<PathBuf, Box<dyn Error>> {
        let take_id = self.session.project().takes()[index].id().to_owned();
        let rendered = render_take_preview(self.session.project(), &take_id, preset)?;
        let path = rendered.path;
        self.session
            .set_take_render(&take_id, preset, rendered.relative_path)?;
        self.project_dirty = true;
        if !self.save_project() {
            return Err("cannot save rebuilt take".into());
        }
        Ok(path)
    }

    fn adjust_key(&mut self, delta: i8) {
        let current = self.session.project().key_shift_semitones();
        self.set_key((current + delta).clamp(-6, 6));
    }

    fn reset_key(&mut self) {
        self.set_key(0);
    }

    fn set_key(&mut self, semitones: i8) {
        if semitones == self.session.project().key_shift_semitones() {
            self.recording_message = Some(format!("Key {}", format_key(semitones)));
            return;
        }
        if let Err(error) = self.session.set_key_shift_semitones(semitones) {
            self.recording_message = Some(error.to_string());
            return;
        }
        self.project_dirty = true;
        let playback_warning = self.playback.set_key_shift(semitones).err();
        let _ = self.save_project();
        self.recording_message = Some(playback_warning.map_or_else(
            || {
                format!(
                    "Key {} · saved; old take will rebuild before playback",
                    format_key(semitones)
                )
            },
            |error| format!("Key {} · playback error: {error}", format_key(semitones)),
        ));
    }

    fn arm(&mut self) {
        let result = self
            .session
            .arm()
            .map_err(|error| -> Box<dyn Error> { Box::new(error) })
            .and_then(|()| self.playback.prepare_recording());
        match result {
            Ok(()) => self.recording_message = Some("armed · press Enter to record".into()),
            Err(error) => {
                if self.session.state() == RecordingState::Armed {
                    let _ = self.session.cancel();
                }
                self.recording_message = Some(error.to_string());
            }
        }
    }

    fn cancel_arm(&mut self) {
        match self.session.cancel() {
            Ok(()) => self.recording_message = Some("recording cancelled".into()),
            Err(error) => self.recording_message = Some(error.to_string()),
        }
    }

    fn start_recording(&mut self) {
        let paths = match recording_paths(self.session.project()) {
            Ok(paths) => paths,
            Err(error) => {
                self.recording_message = Some(error.to_string());
                return;
            }
        };
        let backing_path = match self.playback.accompaniment_path() {
            Ok(path) => path,
            Err(error) => {
                self.recording_message = Some(error.to_string());
                return;
            }
        };
        let backing_key_shift_semitones = self.session.project().key_shift_semitones();
        let recorder = match AudioRecorder::start(
            &paths.dry_temporary_path,
            self.playback.monitor_player(),
            self.monitoring_enabled,
        ) {
            Ok(recorder) => recorder,
            Err(error) => {
                self.recording_message = Some(error.to_string());
                return;
            }
        };
        let device = recorder.device().to_owned();
        if let Err(error) = self.playback.play_from_start() {
            let _ = recorder.stop();
            let _ = fs::remove_file(&paths.dry_temporary_path);
            self.recording_message = Some(error.to_string());
            return;
        }
        if let Err(error) = self.session.start() {
            let _ = recorder.stop();
            let _ = fs::remove_file(&paths.dry_temporary_path);
            self.recording_message = Some(error.to_string());
            return;
        }
        self.active_recording = Some(ActiveRecording {
            recorder,
            id: paths.id,
            dry_relative_path: paths.dry_relative_path,
            dry_temporary_path: paths.dry_temporary_path,
            dry_final_path: paths.dry_final_path,
            mix_relative_path: paths.mix_relative_path,
            mix_temporary_path: paths.mix_temporary_path,
            mix_final_path: paths.mix_final_path,
            backing_path,
            backing_key_shift_semitones,
            timeline: vec![RecordingTimelineAnchor {
                capture_frame: 0,
                song_position: Duration::ZERO,
            }],
        });
        let monitoring = if self.monitoring_enabled {
            "on · use headphones"
        } else {
            "off"
        };
        self.recording_message = Some(format!(
            "recording microphone: {device} · monitoring: {monitoring}"
        ));
    }

    fn finish_recording(&mut self) -> bool {
        let Some(active) = self.active_recording.take() else {
            self.recording_message = Some("active recorder is unavailable".into());
            let _ = self.session.abort();
            return false;
        };
        let summary = match active.recorder.stop() {
            Ok(summary) => summary,
            Err(error) => {
                let _ = fs::remove_file(&active.dry_temporary_path);
                let _ = self.session.abort();
                self.recording_message = Some(error.to_string());
                return false;
            }
        };
        if let Err(error) = place_recording_on_timeline(
            &active.dry_temporary_path,
            &active.dry_final_path,
            &active.timeline,
        ) {
            let _ = self.session.abort();
            self.recording_message = Some(format!(
                "cannot place recording on song timeline: {}; raw audio remains at {}",
                error,
                active.dry_temporary_path.display()
            ));
            return false;
        }
        let dry_project_path = match ProjectPath::new(active.dry_relative_path) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.session.abort();
                self.recording_message = Some(error.to_string());
                return false;
            }
        };
        let mix_result = render_take_mix(
            &active.backing_path,
            &active.dry_final_path,
            &active.mix_temporary_path,
            self.session.project().latency_compensation_ms(),
            active.backing_key_shift_semitones,
            self.default_effect,
        )
        .and_then(|()| {
            fs::rename(&active.mix_temporary_path, &active.mix_final_path).map_err(Into::into)
        });
        let mut take =
            Take::new(active.id, dry_project_path).with_effect_preset(self.default_effect);
        let mix_warning = match mix_result {
            Ok(()) => match ProjectPath::new(active.mix_relative_path) {
                Ok(path) => {
                    take = take
                        .with_mix_audio_at_key(path, self.session.project().key_shift_semitones());
                    None
                }
                Err(error) => Some(error.to_string()),
            },
            Err(error) => {
                let _ = fs::remove_file(&active.mix_temporary_path);
                Some(format!("cannot render mix preview: {error}"))
            }
        };
        let mix_saved = mix_warning.is_none();
        if let Err(error) = self.session.stop(take) {
            self.recording_message = Some(error.to_string());
            return false;
        }
        self.selected_take = self.session.project().takes().len().checked_sub(1);
        self.project_dirty = true;

        let warning = summary
            .warning
            .into_iter()
            .chain(mix_warning)
            .collect::<Vec<_>>();
        let warning = if warning.is_empty() {
            String::new()
        } else {
            format!(" · warning: {}", warning.join("; "))
        };
        self.recording_message = Some(format!(
            "saved {:.1}s {} from {}{}",
            summary.duration.as_secs_f32(),
            if mix_saved {
                "dry + mix take"
            } else {
                "dry take"
            },
            summary.device,
            warning
        ));
        self.save_project()
    }

    fn save_project(&mut self) -> bool {
        if !self.project_dirty {
            return true;
        }
        match FileProjectRepository.save(self.session.project_mut()) {
            Ok(()) => {
                self.project_dirty = false;
                true
            }
            Err(error) => {
                self.recording_message = Some(format!("cannot save project: {error}"));
                false
            }
        }
    }
}

pub fn open(
    project: Project,
    startup_message: Option<String>,
    auto_download_lyrics: bool,
    netease_fallback: bool,
) -> Result<(), Box<dyn Error>> {
    let mut app = app_for_project(
        project,
        startup_message,
        VocalEffectPreset::Clean,
        auto_download_lyrics,
        netease_fallback,
        &mut |progress| eprintln!("{progress}"),
    )?;
    let mut guard = TerminalGuard::enter()?;

    loop {
        app.poll_lyrics_search();
        app.playback.refresh_stream_error();
        guard.terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && should_handle_key(&key)
            && app.handle_key(key.code)
        {
            break;
        }
    }
    Ok(())
}

pub fn open_library(config: LibraryConfig) -> Result<(), Box<dyn Error>> {
    let mut library = MediaLibrary::new(config)?;
    let mut current: Option<App> = None;
    let mut guard = TerminalGuard::enter()?;

    loop {
        if let Some(app) = &mut current {
            app.poll_lyrics_search();
            app.playback.refresh_stream_error();
        }
        poll_netease(&mut library)?;
        poll_import_job(&mut library)?;
        guard
            .terminal
            .draw(|frame| draw_library(frame, &library, current.as_ref()))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && should_handle_key(&key)
            && handle_library_key(&mut library, &mut current, key.code)?
        {
            break;
        }
    }
    Ok(())
}

fn open_library_project(config: &LibraryConfig, path: &Path) -> Result<App, Box<dyn Error>> {
    let repository = FileProjectRepository;
    let project = repository.open(path)?;
    app_for_project(
        project,
        None,
        config.recording.default_effect,
        config.lyrics.auto_download,
        config.lyrics.netease_fallback,
        &mut |_| {},
    )
}

fn app_for_project(
    mut project: Project,
    startup_message: Option<String>,
    default_effect: VocalEffectPreset,
    auto_download_lyrics: bool,
    netease_fallback: bool,
    progress: &mut dyn FnMut(&LyricsProgress),
) -> Result<App, Box<dyn Error>> {
    let mut picker = None;
    let mut lyrics_origin = None;
    let lyrics_message = if auto_download_lyrics {
        match find_missing_lyrics(&mut project, netease_fallback, progress) {
            Ok(LyricsSearch::AlreadyPresent) => {
                FileProjectRepository.save(&mut project)?;
                None
            }
            Ok(LyricsSearch::Candidates(mut choices)) if choices.len() == 1 => {
                let (saved, relative_path) =
                    save_lyrics_choice(&project, choices.remove(0), progress)?;
                project.set_lyrics(relative_path)?;
                FileProjectRepository.save(&mut project)?;
                lyrics_origin = Some(saved.origin.clone());
                Some(format!(
                    "Downloaded lyrics: {} - {} · {}",
                    saved.artist, saved.track, saved.origin
                ))
            }
            Ok(LyricsSearch::Candidates(choices)) => {
                let count = choices.len();
                picker = Some(LyricsPicker {
                    choices,
                    selected: 0,
                });
                Some(format!(
                    "Found {count} lyric matches · choose one before downloading"
                ))
            }
            Ok(LyricsSearch::NotFound) => Some("No duration-matched synced lyrics found".into()),
            Err(error) => Some(format!("Automatic lyric search failed: {error}")),
        }
    } else {
        None
    };
    let lyrics = load_lyrics(&project)?;
    let mut app = App::new(
        project,
        lyrics,
        lyrics_message.or(startup_message),
        default_effect,
    );
    app.lyrics_origin = lyrics_origin;
    app.lyrics_picker = picker;
    app.lyrics_sources = if netease_fallback {
        LyricsSources::LrclibWithNetease
    } else {
        LyricsSources::Lrclib
    };
    Ok(app)
}

fn poll_import_job(library: &mut MediaLibrary) -> Result<(), Box<dyn Error>> {
    let outcome = match library.job.as_ref().map(|job| job.result.try_recv()) {
        Some(Ok(result)) => Some(result),
        Some(Err(TryRecvError::Disconnected)) => {
            Some(Err("Separation task thread exited unexpectedly".to_owned()))
        }
        Some(Err(TryRecvError::Empty)) | None => None,
    };
    let Some(outcome) = outcome else {
        return Ok(());
    };
    if let Some(mut job) = library.job.take()
        && let Some(worker) = job.worker.take()
    {
        let _ = worker.join();
    }
    library.refresh()?;
    let mut message = match outcome {
        Ok(path) => {
            if let Some(index) = library
                .snapshot
                .projects
                .iter()
                .position(|entry| entry.path == path)
            {
                library.project_selected = index;
            }
            format!("Separation complete: {}", path.display())
        }
        Err(error) => format!("Separation failed: {error}"),
    };
    if let Some(next) = library.start_next_queued() {
        message.push_str("\nQueue continues: ");
        message.push_str(&display_name(&next));
    }
    library.message = Some(message);
    Ok(())
}

fn handle_library_key(
    library: &mut MediaLibrary,
    current: &mut Option<App>,
    key: KeyCode,
) -> Result<bool, Box<dyn Error>> {
    if library.confirm_exit {
        return Ok(confirm_library_exit(library, current, key));
    }
    if let Some(app) = current
        && app.pending_take_delete.is_some()
    {
        return Ok(app.handle_key(key));
    }
    if library.music_source == MusicSource::Netease
        && library
            .netease
            .as_ref()
            .is_some_and(|panel| panel.modal.is_some())
    {
        handle_netease_modal_key(library, key)?;
        return Ok(false);
    }
    if key == KeyCode::Char('q')
        && (library.job.is_some()
            || !library.queue.is_empty()
            || library
                .netease
                .as_ref()
                .is_some_and(NeteasePanel::active_download))
    {
        library.confirm_exit = true;
        return Ok(false);
    }
    if let Some(app) = current
        && (app.lyrics_picker.is_some() || app.lyrics_query_editor.is_some())
    {
        app.handle_key(key);
        return Ok(false);
    }
    match key {
        KeyCode::Tab => library.focus = library.focus.next(),
        KeyCode::BackTab => library.focus = library.focus.previous(),
        KeyCode::Char('n')
            if library.focus == LibraryFocus::Sources && library.netease.is_some() =>
        {
            toggle_music_source(library)?;
        }
        KeyCode::Char('r')
            if library.focus != LibraryFocus::Project
                && library.music_source == MusicSource::Local =>
        {
            library.refresh()?;
            library.message = Some("Media library refreshed".into());
        }
        KeyCode::Char('q') if library.focus != LibraryFocus::Project => {
            if let Some(app) = current
                && !app.save_project()
            {
                return Ok(false);
            }
            return Ok(true);
        }
        _ => match library.focus {
            LibraryFocus::Projects => handle_library_project_key(library, current, key)?,
            LibraryFocus::Sources => {
                if library.music_source == MusicSource::Local {
                    handle_library_source_key(library, current, key)?;
                } else {
                    handle_netease_source_key(library, key)?;
                }
            }
            LibraryFocus::Project => {
                if let Some(app) = current {
                    return Ok(app.handle_key(key));
                }
            }
        },
    }
    Ok(false)
}

fn handle_library_project_key(
    library: &mut MediaLibrary,
    current: &mut Option<App>,
    key: KeyCode,
) -> Result<(), Box<dyn Error>> {
    match key {
        KeyCode::Up => {
            library.project_selected = library.project_selected.saturating_sub(1);
        }
        KeyCode::Down => {
            library.project_selected = (library.project_selected + 1)
                .min(library.snapshot.projects.len().saturating_sub(1));
        }
        KeyCode::Enter => {
            if current
                .as_ref()
                .is_some_and(|app| app.session.state() != RecordingState::Idle)
            {
                library.message = Some("Stop or cancel recording before switching projects".into());
            } else if let Some(entry) = library.snapshot.projects.get(library.project_selected) {
                if library.project_is_being_separated(&entry.path) {
                    library.message =
                        Some("Wait for separation to finish before opening this project".into());
                } else {
                    if let Some(app) = current
                        && !app.save_project()
                    {
                        return Ok(());
                    }
                    *current = Some(open_library_project(&library.config, &entry.path)?);
                    library.message = Some(format!("Opened: {}", entry.title));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn confirm_library_exit(
    library: &mut MediaLibrary,
    current: &mut Option<App>,
    key: KeyCode,
) -> bool {
    library.confirm_exit = false;
    if !matches!(key, KeyCode::Char('y' | 'Y')) {
        return false;
    }
    if let Some(app) = current
        && !app.save_project()
    {
        return false;
    }
    library.cancel_imports();
    if let Some(panel) = &mut library.netease {
        panel.cancel_all();
    }
    true
}

fn handle_library_source_key(
    library: &mut MediaLibrary,
    current: &mut Option<App>,
    key: KeyCode,
) -> Result<(), Box<dyn Error>> {
    match key {
        KeyCode::Up => {
            library.source_selected = library.source_selected.saturating_sub(1);
        }
        KeyCode::Down => {
            library.source_selected =
                (library.source_selected + 1).min(library.snapshot.sources.len().saturating_sub(1));
        }
        KeyCode::Enter => {
            if let Some(source) = library.snapshot.sources.get(library.source_selected)
                && source.project_state == SourceProjectState::Current
            {
                if current
                    .as_ref()
                    .is_some_and(|app| app.session.state() != RecordingState::Idle)
                {
                    library.message =
                        Some("Stop or cancel recording before switching projects".into());
                } else {
                    *current = Some(open_library_project(&library.config, &source.project_path)?);
                    if let Some(index) = library
                        .snapshot
                        .projects
                        .iter()
                        .position(|entry| entry.path == source.project_path)
                    {
                        library.project_selected = index;
                    }
                    library.message = Some(format!("Opened: {}", source.project_path.display()));
                }
            } else {
                library.start_import();
            }
        }
        KeyCode::Char('s') => start_source_reseparation(library, current),
        _ => {}
    }
    Ok(())
}

fn start_source_reseparation(library: &mut MediaLibrary, current: &mut Option<App>) {
    let selected = library
        .snapshot
        .sources
        .get(library.source_selected)
        .cloned();
    let replacing_open = selected.as_ref().is_some_and(|source| {
        source.project_state == SourceProjectState::Current
            && current
                .as_ref()
                .is_some_and(|app| app.session.project().root() == source.project_path)
    });
    if replacing_open
        && current
            .as_ref()
            .is_some_and(|app| app.session.state() != RecordingState::Idle)
    {
        library.message = Some("Stop or cancel recording before re-separating".into());
        return;
    }
    if replacing_open {
        if let Some(app) = current
            && !app.save_project()
        {
            return;
        }
        *current = None;
    }
    library.start_import();
}

fn should_handle_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

fn draw(frame: &mut Frame, app: &App) {
    draw_project(frame, frame.area(), app, None);
    if let Some(editor) = &app.lyrics_query_editor {
        draw_lyrics_query_editor(frame, frame.area(), editor);
    } else if let Some(picker) = &app.lyrics_picker {
        draw_lyrics_picker(frame, frame.area(), picker);
    }
}

fn draw_project(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    app: &App,
    library_focused: Option<bool>,
) {
    let playback = app.playback.snapshot();
    let position = playback.position;
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(5),
            Constraint::Length(5),
        ])
        .split(area);
    let project = app.session.project();
    let playback_status = match playback.status {
        PlaybackStatus::Unavailable => "unavailable",
        PlaybackStatus::Loading => "loading",
        PlaybackStatus::Paused => "paused",
        PlaybackStatus::Playing => "playing",
        PlaybackStatus::Finished => "finished",
        PlaybackStatus::Error => "error",
    };
    let duration = playback.duration;
    let volume = playback.volume;
    let track = playback
        .track
        .map_or("unavailable", SessionTrackKind::label);
    let current_effect = if app.session.state() == RecordingState::Idle {
        app.selected_take.map_or(app.default_effect, |index| {
            project.takes()[index].effect_preset()
        })
    } else {
        app.default_effect
    };
    let take_status = app.selected_take.map_or_else(
        || "selected take: none".to_owned(),
        |index| format!("selected take: {}/{}", index + 1, project.takes().len()),
    );
    let audio_line = if let Some(error) = playback.error.as_deref() {
        format!("audio: {track} · error: {error}")
    } else {
        format!(
            "audio: {} · {} · {} / {} · volume {:.0}% · Key {}",
            track,
            playback_status,
            format_duration(position),
            duration.map_or_else(|| "--:--".into(), format_duration),
            volume * 100.0,
            format_key(project.key_shift_semitones()),
        )
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                project.title(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  [{}]", project.id())),
        ]),
        Line::from(format!("source: {}", project.source().as_str())),
        Line::from(format!(
            "separation: {}  recording: {:?}  takes: {}  {}",
            project.separation(),
            app.session.state(),
            project.takes().len(),
            take_status
        )),
        Line::from(audio_line),
        Line::from(
            app.recording_message
                .as_deref()
                .unwrap_or("recording: press a to arm"),
        ),
    ])
    .block(
        Block::default()
            .title(panel_title(
                &format!("K3 · FX: {}", current_effect.label()),
                library_focused,
            ))
            .borders(Borders::ALL)
            .border_style(panel_border_style(library_focused)),
    );
    frame.render_widget(header, areas[0]);

    let visible_rows = usize::from(areas[1].height.saturating_sub(2));
    let (lyric_lines, lyric_progress) =
        lyrics_for_display(app.lyrics.as_ref(), position, visible_rows);
    let lyric_title = lyrics_panel_title(app, position, &lyric_progress);
    let lyric_panel = Paragraph::new(lyric_lines)
        .block(
            Block::default()
                .title(lyric_title)
                .borders(Borders::ALL)
                .border_style(panel_border_style(library_focused)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(lyric_panel, areas[1]);

    draw_project_footer(frame, areas[2], app, library_focused);
    if let Some(take_id) = &app.pending_take_delete {
        let popup = centered_popup(area, 58, 8);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!("Take: {take_id}")),
                Line::from("Remove its dry recording and mix audio?"),
                Line::from(""),
                Line::from("Enter / Esc: Keep (default)    y: Delete"),
            ])
            .block(
                Block::default()
                    .title("Delete recorded take")
                    .borders(Borders::ALL),
            ),
            popup,
        );
    }
}

fn lyrics_panel_title(app: &App, position: Duration, progress: &str) -> String {
    let origin = app
        .lyrics_origin
        .as_deref()
        .map_or_else(String::new, |origin| format!(" · {origin}"));
    format!(" Lyrics{origin} · {}{progress} ", format_duration(position))
}

fn draw_lyrics_picker(frame: &mut Frame, area: Rect, picker: &LyricsPicker) {
    let width = area.width.saturating_sub(2).clamp(1, 100);
    let height = area.height.saturating_sub(2).clamp(1, 24);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height.min(area.height),
    );
    let maximum_choice_rows = usize::from(popup.height.saturating_sub(5).clamp(1, 6));
    let visible_rows = picker.choices.len().min(maximum_choice_rows);
    let choice_height = (u16::try_from(visible_rows).unwrap_or(u16::MAX) + 2)
        .min(popup.height.saturating_sub(3).max(1));
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(choice_height), Constraint::Min(3)])
        .split(popup);
    let start = picker
        .selected
        .saturating_add(1)
        .saturating_sub(visible_rows);
    let lines = picker
        .choices
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_rows)
        .map(|(index, choice)| {
            let selected = index == picker.selected;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let label_width = usize::from(sections[0].width.saturating_sub(4));
            let label = fit_text_end(&choice.label(), label_width);
            Line::from(Span::styled(format!("{marker}{label}"), style))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" Results · ↑/↓ choose · Enter download · Esc skip ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        sections[0],
    );

    let preview_rows = usize::from(sections[1].height.saturating_sub(2));
    let (preview_title, preview) = picker.choices.get(picker.selected).map_or_else(
        || {
            (
                "Preview".to_owned(),
                vec![Line::from("No preview available")],
            )
        },
        |choice| {
            let title = format!("Preview · {}", choice.origin_label());
            let lines = choice
                .preview_lines(preview_rows)
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>();
            (title, lines)
        },
    );
    frame.render_widget(
        Paragraph::new(preview)
            .block(
                Block::default()
                    .title(format!(" {preview_title} "))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false }),
        sections[1],
    );
}

fn draw_lyrics_query_editor(frame: &mut Frame, area: Rect, editor: &LyricsQueryEditor) {
    let width = area.width.saturating_sub(2).clamp(1, 80);
    let height = 3.min(area.height);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let inner_width = usize::from(popup.width.saturating_sub(2).max(1));
    let cursor_byte = char_index_to_byte(&editor.query, editor.cursor);
    let cursor_width = UnicodeWidthStr::width(&editor.query[..cursor_byte]);
    let scroll = cursor_width.saturating_sub(inner_width.saturating_sub(1));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(editor.query.as_str())
            .block(
                Block::default()
                    .title(" Search lyrics · Enter search · Esc cancel ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .scroll((0, u16::try_from(scroll).unwrap_or(u16::MAX))),
        popup,
    );
    frame.set_cursor_position((
        popup.x + 1 + u16::try_from(cursor_width.saturating_sub(scroll)).unwrap_or(u16::MAX),
        popup.y + 1,
    ));
}

#[derive(Clone, Copy)]
enum FooterAction {
    Playback,
    Seek,
    Restart,
    Volume,
    SourceTrack,
    Take,
    SelectTake,
    Effect,
    Key,
    Lyrics,
    Arm,
    RecordToggle,
    Monitor,
    Quit,
}

fn mode_allows_footer_action(state: RecordingState, action: FooterAction) -> bool {
    match action {
        FooterAction::Playback | FooterAction::Restart => state != RecordingState::Recording,
        FooterAction::Take
        | FooterAction::SelectTake
        | FooterAction::Effect
        | FooterAction::Key
        | FooterAction::Lyrics
        | FooterAction::Arm => state == RecordingState::Idle,
        FooterAction::RecordToggle => state != RecordingState::Idle,
        FooterAction::Seek
        | FooterAction::Volume
        | FooterAction::SourceTrack
        | FooterAction::Monitor
        | FooterAction::Quit => true,
    }
}

fn draw_project_footer(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    app: &App,
    library_focused: Option<bool>,
) {
    let state = app.session.state();
    let audio = app.playback.has_audio();
    let has_track = |kind| app.playback.has_track(kind);
    let has_take = app.selected_take.is_some();
    let lyrics_searching = app.lyrics_search_job.is_some() || app.lyrics_query_editor.is_some();
    let mode = |action| mode_allows_footer_action(state, action);
    let footer = Paragraph::new(vec![
        footer_line(&[
            ("Space play/pause", audio && mode(FooterAction::Playback)),
            (
                "←/→ seek 5s (by lyrics while recording)",
                audio
                    && mode(FooterAction::Seek)
                    && (state != RecordingState::Recording || app.lyrics.is_some()),
            ),
            ("r restart", audio && mode(FooterAction::Restart)),
            ("-/+ volume", audio && mode(FooterAction::Volume)),
        ]),
        footer_line(&[
            (
                "1 original",
                has_track(SessionTrackKind::Original) && mode(FooterAction::SourceTrack),
            ),
            (
                "2 accompaniment",
                has_track(SessionTrackKind::Accompaniment) && mode(FooterAction::SourceTrack),
            ),
            (
                "3 vocals (switchable while recording)",
                has_track(SessionTrackKind::Vocals) && mode(FooterAction::SourceTrack),
            ),
            ("4 take", has_take && mode(FooterAction::Take)),
            ("q quit", mode(FooterAction::Quit)),
        ]),
        footer_line(&[
            (
                "[/] select take",
                has_take && mode(FooterAction::SelectTake),
            ),
            ("e+1..5 effect", has_take && mode(FooterAction::Effect)),
            (
                "Del delete take",
                has_take && mode(FooterAction::SelectTake),
            ),
            ("/ reset Key", mode(FooterAction::Key)),
            ("l lyrics", mode(FooterAction::Lyrics) && !lyrics_searching),
            (
                "a arm",
                audio
                    && has_track(SessionTrackKind::Accompaniment)
                    && mode(FooterAction::Arm)
                    && !lyrics_searching,
            ),
            ("Enter start/stop", mode(FooterAction::RecordToggle)),
            ("m monitor", mode(FooterAction::Monitor)),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(panel_border_style(library_focused)),
    );
    frame.render_widget(footer, area);
}

fn footer_line(items: &[(&'static str, bool)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(items.len().saturating_mul(2).saturating_sub(1));
    for (index, (text, enabled)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" · "));
        }
        let style = if *enabled {
            Style::default()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(*text, style));
    }
    Line::from(spans)
}

fn panel_border_style(library_focused: Option<bool>) -> Style {
    match library_focused {
        Some(true) => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Some(false) => Style::default().fg(Color::DarkGray),
        None => Style::default(),
    }
}

fn panel_title(label: &str, library_focused: Option<bool>) -> String {
    if library_focused == Some(true) {
        format!(" ▶ {label} ")
    } else {
        format!(" {label} ")
    }
}

fn draw_library(frame: &mut Frame, library: &MediaLibrary, current: Option<&App>) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(56),
            Constraint::Percentage(22),
        ])
        .split(frame.area());
    draw_library_projects(frame, columns[0], library);
    if let Some(app) = current {
        draw_project(
            frame,
            columns[1],
            app,
            Some(library.focus == LibraryFocus::Project),
        );
        if let Some(editor) = &app.lyrics_query_editor {
            draw_lyrics_query_editor(frame, columns[1], editor);
        } else if let Some(picker) = &app.lyrics_picker {
            draw_lyrics_picker(frame, columns[1], picker);
        }
    } else {
        let focused = library.focus == LibraryFocus::Project;
        frame.render_widget(
            Paragraph::new(
                "No project is open\n\nTab to Music, select a file, then press Enter to separate",
            )
            .block(
                Block::default()
                    .title(panel_title("K3", Some(focused)))
                    .borders(Borders::ALL)
                    .border_style(panel_border_style(Some(focused))),
            )
            .wrap(Wrap { trim: false }),
            columns[1],
        );
    }
    draw_library_sources(frame, columns[2], library);
    draw_netease_modal(frame, library);
}

fn draw_library_projects(frame: &mut Frame, area: ratatui::layout::Rect, library: &MediaLibrary) {
    let rows = library
        .snapshot
        .projects
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            library_row(
                &entry.title,
                index == library.project_selected,
                library.focus == LibraryFocus::Projects,
            )
        })
        .collect::<Vec<_>>();
    let scroll = wrapped_list_scroll(&rows, library.project_selected, area.width, area.height);
    let title = if library.focus == LibraryFocus::Projects {
        "▶ Projects · Enter open"
    } else {
        "Projects"
    };
    let focused = library.focus == LibraryFocus::Projects;
    frame.render_widget(
        Paragraph::new(if rows.is_empty() {
            vec![Line::from("No projects")]
        } else {
            rows
        })
        .block(
            Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL)
                .border_style(panel_border_style(Some(focused))),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_library_sources(frame: &mut Frame, area: ratatui::layout::Rect, library: &MediaLibrary) {
    if library.music_source == MusicSource::Netease {
        draw_netease_sources(frame, area, library);
        return;
    }
    let failed = library
        .message
        .as_deref()
        .is_some_and(|message| message.starts_with("Separation failed"));
    let status_height = if library.message.is_some() { 6 } else { 0 };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(status_height)])
        .split(area);
    let list_area = areas[0];
    let inner_width = usize::from(list_area.width.saturating_sub(2));
    let name_width = inner_width.saturating_sub(4);
    let rows = library
        .snapshot
        .sources
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let state = if library
                .job
                .as_ref()
                .is_some_and(|job| job.source == entry.path)
            {
                separation_spinner_frame()
            } else if library
                .queue
                .iter()
                .any(|request| request.source.path == entry.path)
            {
                "◷"
            } else {
                match entry.project_state {
                    SourceProjectState::New => "+",
                    SourceProjectState::Current => "✓",
                    SourceProjectState::ReplaceSource => "↻",
                }
            };
            let selected = index == library.source_selected;
            let full_name = display_name(&entry.path);
            let name = if selected {
                full_name
            } else {
                fit_source_name(&full_name, name_width)
            };
            library_row(
                &format!("{state} {name}"),
                selected,
                library.focus == LibraryFocus::Sources,
            )
        })
        .collect::<Vec<_>>();
    let scroll = wrapped_list_scroll(
        &rows,
        library.source_selected,
        list_area.width,
        list_area.height,
    );
    let switch = if library.netease.is_some() {
        " · n NetEase"
    } else {
        ""
    };
    let title = if library.focus == LibraryFocus::Sources {
        format!(
            "▶ Music · Enter open/separate · s re-separate · queued {}{switch}",
            library.queue.len(),
        )
    } else {
        format!("Music · queued {}{switch}", library.queue.len())
    };
    let focused = library.focus == LibraryFocus::Sources;
    frame.render_widget(
        Paragraph::new(if rows.is_empty() {
            vec![Line::from("No audio files")]
        } else {
            rows
        })
        .block(
            Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL)
                .border_style(panel_border_style(Some(focused))),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false }),
        list_area,
    );

    if let Some(message) = &library.message {
        draw_library_source_message(frame, areas[1], message, failed);
    }
}

fn centered_popup(area: Rect, requested_width: u16, requested_height: u16) -> Rect {
    let width = requested_width.min(area.width).max(1);
    let height = requested_height.min(area.height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn draw_library_source_message(frame: &mut Frame, area: Rect, message: &str, failed: bool) {
    let color = if failed { Color::Red } else { Color::Cyan };
    let title = if failed { "Error" } else { "Status" };
    frame.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(color))
            .block(
                Block::default()
                    .title(format!(" {title} "))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn separation_spinner_frame() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let frame = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() / 100);
    FRAMES[(frame % FRAMES.len() as u128) as usize]
}

fn library_row(text: &str, selected: bool, focused: bool) -> Line<'static> {
    let marker = if selected { "▶ " } else { "  " };
    Line::from(Span::styled(
        format!("{marker}{text}"),
        library_row_style(selected, focused),
    ))
}

fn fitted_unselected_library_row(text: &str, width: usize) -> Line<'static> {
    Line::from(fit_single_line_middle(format!("  {text}"), width))
}

fn library_row_style(selected: bool, focused: bool) -> Style {
    if selected && focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn wrapped_list_scroll(rows: &[Line<'_>], selected: usize, width: u16, height: u16) -> u16 {
    let Some(selected_row) = rows.get(selected) else {
        return 0;
    };
    let inner_width = width.saturating_sub(2).max(1);
    let visible = usize::from(height.saturating_sub(2)).max(1);
    let wrap = Wrap { trim: false };
    let preceding_lines = Paragraph::new(rows[..selected].to_vec())
        .wrap(wrap)
        .line_count(inner_width);
    let selected_lines = Paragraph::new(vec![selected_row.clone()])
        .wrap(wrap)
        .line_count(inner_width);
    u16::try_from(
        preceding_lines
            .saturating_add(selected_lines)
            .saturating_sub(visible),
    )
    .unwrap_or(u16::MAX)
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |value| value.to_string_lossy().into_owned(),
    )
}

fn fit_source_name(name: &str, width: usize) -> String {
    if usize::from(name.cell_width()) <= width {
        return name.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".into();
    }

    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{extension}"))
        .filter(|extension| usize::from(extension.as_str().cell_width()) + 1 < width)
        .unwrap_or_default();
    let suffix_width = usize::from(extension.as_str().cell_width());
    let prefix_width = width.saturating_sub(suffix_width + 1);
    format!("{}…{extension}", take_prefix_width(name, prefix_width))
}

fn fit_single_line_middle(value: String, width: usize) -> String {
    if usize::from(value.as_str().cell_width()) <= width {
        return value;
    }
    let omission = "...";
    let omission_width = usize::from(omission.cell_width());
    if width <= omission_width {
        return take_prefix_width(omission, width);
    }
    let content_width = width - omission_width;
    let prefix_width = content_width.div_ceil(2);
    let suffix_width = content_width - prefix_width;
    format!(
        "{}{}{}",
        take_prefix_width(&value, prefix_width),
        omission,
        take_suffix_width(&value, suffix_width)
    )
}

fn take_prefix_width(value: &str, maximum_width: usize) -> String {
    let mut width = 0;
    let span = Span::raw(value);
    span.styled_graphemes(Style::default())
        .take_while(|grapheme| {
            let grapheme_width = usize::from(grapheme.symbol.cell_width());
            if width + grapheme_width > maximum_width {
                return false;
            }
            width += grapheme_width;
            true
        })
        .map(|grapheme| grapheme.symbol)
        .collect()
}

fn take_suffix_width(value: &str, maximum_width: usize) -> String {
    let span = Span::raw(value);
    let graphemes = span
        .styled_graphemes(Style::default())
        .map(|grapheme| grapheme.symbol)
        .collect::<Vec<_>>();
    let mut width = 0;
    let mut start = graphemes.len();
    for (index, grapheme) in graphemes.iter().enumerate().rev() {
        let grapheme_width = usize::from(grapheme.cell_width());
        if width + grapheme_width > maximum_width {
            break;
        }
        width += grapheme_width;
        start = index;
    }
    graphemes[start..].concat()
}

fn char_index_to_byte(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map_or(value.len(), |(byte, _)| byte)
}

fn fit_text_end(value: &str, width: usize) -> String {
    if usize::from(value.cell_width()) <= width {
        return value.to_owned();
    }
    fit_text_with_suffix(value, width, "…")
}

fn fit_text_with_suffix(value: &str, width: usize, suffix: &str) -> String {
    let suffix_width = usize::from(suffix.cell_width());
    if width <= suffix_width {
        return take_prefix_width(suffix, width);
    }
    format!(
        "{}{}",
        take_prefix_width(value, width - suffix_width),
        suffix
    )
}

fn effect_preset_for_key(key: KeyCode) -> Option<VocalEffectPreset> {
    match key {
        KeyCode::Char('1') => Some(VocalEffectPreset::Clean),
        KeyCode::Char('2') => Some(VocalEffectPreset::Studio),
        KeyCode::Char('3') => Some(VocalEffectPreset::Ktv),
        KeyCode::Char('4') => Some(VocalEffectPreset::Theater),
        KeyCode::Char('5') => Some(VocalEffectPreset::Church),
        _ => None,
    }
}

fn format_key(semitones: i8) -> String {
    format!("{semitones:+}")
}

fn lyrics_for_display(
    lyrics: Option<&LyricsTimeline>,
    position: Duration,
    visible_rows: usize,
) -> (Vec<Line<'static>>, String) {
    lyrics.map_or_else(
        || (vec![Line::from("No lyrics loaded")], String::new()),
        |timeline| {
            let window = lyric_window(timeline, position, visible_rows);
            let countdown = lyric_countdown(timeline, position);
            let lines = window
                .range
                .clone()
                .map(|index| {
                    let is_current = window.current == Some(index);
                    let is_past = window.current.is_some_and(|current| index < current);
                    let style = if is_current {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else if is_past {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let marker = if is_current {
                        Span::styled("▶   ", style)
                    } else if countdown.is_some_and(|countdown| countdown.index == index) {
                        let seconds = countdown.expect("countdown was just matched").seconds;
                        Span::styled(
                            format!("  {seconds} "),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw("    ")
                    };
                    let text = &timeline.lines()[index].text;
                    Line::from(vec![
                        marker,
                        Span::styled(if text.is_empty() { "♪" } else { text }.to_owned(), style),
                    ])
                })
                .collect::<Vec<_>>();
            let progress = window.current.map_or_else(String::new, |current| {
                format!(" · {}/{}", current + 1, timeline.lines().len())
            });
            let lines = if lines.is_empty() {
                vec![Line::from("No timed lyrics in this file")]
            } else {
                lines
            };
            (lines, progress)
        },
    )
}

fn lyric_seek_target(
    timeline: &LyricsTimeline,
    position: Duration,
    direction: i8,
) -> Option<Duration> {
    let lines = timeline.lines();
    if direction < 0 {
        let current = lines.partition_point(|line| line.at <= position);
        let target = current.checked_sub(2)?;
        return lines.get(target).map(|line| line.at);
    }
    let next = lines.partition_point(|line| line.at <= position);
    lines.get(next).map(|line| line.at)
}

struct RecordingPaths {
    id: String,
    dry_relative_path: String,
    dry_temporary_path: PathBuf,
    dry_final_path: PathBuf,
    mix_relative_path: String,
    mix_temporary_path: PathBuf,
    mix_final_path: PathBuf,
}

fn recording_paths(project: &Project) -> Result<RecordingPaths, Box<dyn Error>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let id = format!("take-{timestamp}");
    let dry_relative_path = format!("takes/{id}-dry.wav");
    let dry_final_path = project.root().join(Path::new(&dry_relative_path));
    let dry_temporary_path = project.root().join(format!("takes/.{id}-dry.wav.partial"));
    let mix_relative_path = format!("takes/{id}-mix.wav");
    let mix_final_path = project.root().join(Path::new(&mix_relative_path));
    let mix_temporary_path = project.root().join(format!("takes/.{id}-mix.wav.partial"));
    if [
        &dry_final_path,
        &dry_temporary_path,
        &mix_final_path,
        &mix_temporary_path,
    ]
    .into_iter()
    .any(|path| path.exists())
    {
        return Err(format!("recording destination already exists for {id}").into());
    }
    Ok(RecordingPaths {
        id,
        dry_relative_path,
        dry_temporary_path,
        dry_final_path,
        mix_relative_path,
        mix_temporary_path,
        mix_final_path,
    })
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn load_lyrics(project: &Project) -> Result<Option<LyricsTimeline>, io::Error> {
    let Some(relative) = project.lyrics() else {
        return Ok(None);
    };
    match fs::read_to_string(project.root().join(Path::new(relative.as_str()))) {
        Ok(text) => Ok(Some(LyricsTimeline::parse(&text))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut output = stdout();
        if let Err(error) = execute!(output, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(output)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(stdout(), LeaveAlternateScreen);
                Err(error)
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        App, CatalogJob, ChromeLoginJob, FooterAction, ImportJob, LyricsPicker, LyricsQueryEditor,
        ManagedTask, MediaLibrary, NeteaseDownloadJob, NeteaseModal, NeteaseNotice, NeteasePanel,
        NeteaseView, SeparationRequest, SourceProjectState, draw_library_sources,
        effect_preset_for_key, fit_single_line_middle, fit_source_name, format_duration,
        handle_library_key, handle_library_source_key, handle_netease_modal_key,
        handle_netease_source_key, load_lyrics, lyric_countdown, lyric_seek_target, lyric_window,
        mode_allows_footer_action, netease_sign_in_hint, netease_song_label,
        playback_command_for_key, poll_import_job, poll_netease_catalog, poll_netease_chrome_login,
        poll_netease_download, recording_track_for_key, should_handle_key,
    };
    use crate::netease::{NeteaseError, NeteaseSession, Quality, RiskStore, SessionStore, Song};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use k3_app::LyricsChoice;
    use k3_app::{
        LyricCountdown, PlaybackCommand, SessionPlayback, SessionTrackKind,
        TrackKind as SharedTrackKind,
    };
    use k3_core::{
        CreateProject, FileProjectRepository, LyricsTimeline, ProjectRepository, RecordingState,
        VocalEffectPreset,
    };
    use ratatui::{Terminal, backend::TestBackend, style::Color};
    use std::{
        fs,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    #[test]
    fn netease_page_selection_and_all_liked_selection_have_distinct_scopes() {
        let sandbox = tempfile::tempdir().unwrap();
        let mut panel = NeteasePanel::new(
            SessionStore::at(sandbox.path().join("session.json")),
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        panel.view = NeteaseView::Liked;
        panel.songs = (0..75)
            .map(|id| Song {
                id,
                title: format!("Song {id}"),
                artists: vec!["Artist".into()],
                album: "Album".into(),
                cover_url: None,
                max_quality: Quality::Lossless,
                available: true,
            })
            .collect();
        panel.page_offset = 50;

        panel.select_current_page();
        assert_eq!(panel.selected_ids.len(), 25);
        assert!(panel.selected_ids.contains(&50));
        assert!(!panel.selected_ids.contains(&49));

        panel.select_all_liked();
        assert_eq!(panel.selected_ids.len(), 75);
    }

    #[test]
    fn netease_song_rows_show_album_quality_and_unavailable_reason() {
        let song = Song {
            id: 7,
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: false,
        };

        assert_eq!(
            netease_song_label(&song, true),
            "[x] Artist-Song · Album · lossless · unavailable (account or region)"
        );
    }

    #[test]
    fn liked_netease_song_rows_match_local_expansion_style() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let session_store = SessionStore::at(sandbox.path().join("session.json"));
        let session: NeteaseSession = serde_json::from_value(serde_json::json!({
            "cookie": "MUSIC_U=test",
            "user_id": 42,
            "nickname": "Singer"
        }))
        .unwrap();
        session_store.save(&session).unwrap();
        let mut panel = NeteasePanel::new(
            session_store,
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        panel.songs.push(Song {
            id: 7,
            title: "这是一首非常非常长的歌曲TAIL".into(),
            artists: vec!["很长的歌手名字".into()],
            album: "很长的专辑名字".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        });
        panel.songs.push(Song {
            id: 8,
            title: "这是一首非常非常长的歌曲TAIL".into(),
            artists: vec!["很长的歌手名字".into()],
            album: "很长的专辑名字".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        });
        library.netease = Some(panel);
        library.music_source = super::MusicSource::Netease;
        library.focus = super::LibraryFocus::Sources;
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| super::draw_netease_sources(frame, frame.area(), &library))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (1..buffer.area.height.saturating_sub(1))
            .map(|y| {
                (1..buffer.area.width.saturating_sub(1))
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let highlighted = (1..buffer.area.height.saturating_sub(1))
            .flat_map(|y| {
                (1..buffer.area.width.saturating_sub(1)).filter_map(move |x| {
                    buffer.cell((x, y)).and_then(|cell| {
                        (cell.fg == Color::Yellow && !cell.symbol().trim().is_empty())
                            .then(|| cell.symbol().to_owned())
                    })
                })
            })
            .collect::<String>();
        assert!(highlighted.contains("TAIL"), "{highlighted}");
        assert!(highlighted.contains("很长的专辑名字"), "{highlighted}");
        assert!(!highlighted.contains("..."), "{highlighted}");
        let shortened = rows.iter().find(|row| row.contains("...")).unwrap();
        let ellipsis = shortened.find("...").unwrap();
        assert!(shortened[..ellipsis].contains("[ ]"), "{shortened}");
        assert!(
            shortened[ellipsis + 3..].contains("lossless"),
            "{shortened}"
        );

        library.netease.as_mut().unwrap().view = NeteaseView::Search;
        terminal
            .draw(|frame| super::draw_netease_sources(frame, frame.area(), &library))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (1..buffer.area.height.saturating_sub(1))
            .map(|y| {
                (1..buffer.area.width.saturating_sub(1))
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(!rows[0].contains("..."), "{}", rows[0]);
        assert!(!rows[1].trim().is_empty(), "{}", rows[1]);
        assert!(rows.join("").contains("TAIL"), "{rows:?}");
    }

    #[test]
    #[allow(clippy::unicode_not_nfc)]
    fn middle_fitting_uses_terminal_grapheme_widths() {
        assert_eq!(fit_single_line_middle("abcdef".into(), 0), "");
        assert_eq!(fit_single_line_middle("abcdef".into(), 1), ".");
        assert_eq!(fit_single_line_middle("abcdef".into(), 2), "..");
        assert_eq!(fit_single_line_middle("abcdef".into(), 3), "...");
        assert_eq!(fit_single_line_middle("abcdef".into(), 5), "a...f");
        assert_eq!(fit_single_line_middle("ｶﾞ".into(), 1), ".");
        assert_eq!(fit_single_line_middle("👨‍👩‍👧‍👦abcde".into(), 6), "👨‍👩‍👧‍👦...e");
    }

    #[test]
    fn netease_sign_in_hint_prefers_chrome_and_keeps_qr_as_a_fallback() {
        assert_eq!(
            netease_sign_in_hint(),
            "Press c to import Chrome login · i for QR"
        );
    }

    #[test]
    fn corrupt_netease_session_is_reported_instead_of_treated_as_logged_out() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        fs::write(store.path(), b"not json").unwrap();

        let error = NeteasePanel::new(
            store,
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .err()
        .expect("a corrupt session must be rejected");

        assert!(matches!(error, NeteaseError::Json(_)));
    }

    #[test]
    fn netease_zero_failed_summary_is_a_status_not_an_error() {
        assert!(!NeteaseNotice::info("Download complete").is_error());
        assert!(NeteaseNotice::error("Download failed").is_error());

        let mut notice = NeteaseNotice::info("Downloaded Song");
        notice.append_error("Library refresh failed: missing directory");
        assert!(notice.is_error());
        assert!(notice.contains("Library refresh failed"));
    }

    #[test]
    fn completed_netease_download_starts_separation_while_next_download_runs() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        let downloaded = music.join("NetEase/Artist-Song.flac");
        fs::create_dir_all(downloaded.parent().unwrap()).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(&downloaded, b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let panel = library.netease.as_mut().unwrap();
        let completed = Song {
            id: 7,
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        };
        let next = Song {
            id: 8,
            title: "Next".into(),
            ..completed.clone()
        };
        let (sender, result) = mpsc::channel();
        panel.download_job = Some(NeteaseDownloadJob {
            song: completed,
            task: ManagedTask::from_receiver(result),
        });
        panel.download_queue.push_back(next);
        sender
            .send(Ok(crate::netease::DownloadOutcome::Downloaded {
                path: downloaded.clone(),
                quality: Quality::Lossless,
            }))
            .unwrap();

        poll_netease_download(&mut library).unwrap();

        assert_eq!(library.job.as_ref().unwrap().source, downloaded);
        assert!(library.queue.is_empty());
        assert_eq!(
            library
                .netease
                .as_ref()
                .unwrap()
                .download_job
                .as_ref()
                .unwrap()
                .song
                .title,
            "Next"
        );
    }

    #[test]
    fn scan_failure_does_not_stop_separation_or_the_next_netease_download() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        let downloaded = music.join("NetEase/Artist-Song.flac");
        fs::create_dir_all(downloaded.parent().unwrap()).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(&downloaded, b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let completed = Song {
            id: 7,
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        };
        let next = Song {
            id: 8,
            title: "Next".into(),
            ..completed.clone()
        };
        let (sender, result) = mpsc::channel();
        let panel = library.netease.as_mut().unwrap();
        panel.download_job = Some(NeteaseDownloadJob {
            song: completed,
            task: ManagedTask::from_receiver(result),
        });
        panel.download_queue.push_back(next);
        sender
            .send(Ok(crate::netease::DownloadOutcome::Downloaded {
                path: downloaded.clone(),
                quality: Quality::Lossless,
            }))
            .unwrap();
        fs::remove_dir_all(&projects).unwrap();

        poll_netease_download(&mut library).unwrap();

        assert_eq!(library.job.as_ref().unwrap().source, downloaded);
        assert_eq!(
            library
                .netease
                .as_ref()
                .unwrap()
                .download_job
                .as_ref()
                .unwrap()
                .song
                .title,
            "Next"
        );
        assert!(
            library
                .netease_message
                .as_deref()
                .unwrap()
                .contains("Library refresh failed:")
        );
    }

    #[test]
    fn local_separation_can_start_while_netease_download_is_running() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let source = music.join("local.wav");
        fs::write(&source, b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let (_sender, result) = mpsc::channel();
        library.netease.as_mut().unwrap().download_job = Some(NeteaseDownloadJob {
            song: Song {
                id: 7,
                title: "Online".into(),
                artists: vec!["Artist".into()],
                album: "Album".into(),
                cover_url: None,
                max_quality: Quality::Lossless,
                available: true,
            },
            task: ManagedTask::from_receiver(result),
        });

        library.start_import();

        assert_eq!(library.job.as_ref().unwrap().source, source);
        assert!(library.netease.as_ref().unwrap().download_job.is_some());
    }

    #[test]
    fn netease_download_can_start_while_separation_is_running() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let (_sender, result) = mpsc::channel();
        library.job = Some(ImportJob {
            source: sandbox.path().join("active.wav"),
            result,
            cancellation: Arc::new(AtomicBool::new(false)),
            worker: None,
        });
        let panel = library.netease.as_mut().unwrap();
        let song = Song {
            id: 7,
            title: "Online".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        };
        panel.selected_ids.insert(song.id);
        panel.songs.push(song);
        panel.modal = Some(NeteaseModal::ConfirmDownload);

        handle_netease_modal_key(&mut library, KeyCode::Char('y')).unwrap();

        assert!(library.job.is_some());
        assert!(library.netease.as_ref().unwrap().download_job.is_some());
    }

    #[test]
    fn active_netease_download_cannot_be_replaced_by_another_batch() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let active = Song {
            id: 7,
            title: "Active".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        };
        let pending = Song {
            id: 8,
            title: "Pending".into(),
            ..active.clone()
        };
        let replacement = Song {
            id: 9,
            title: "Replacement".into(),
            ..active.clone()
        };
        let (_sender, result) = mpsc::channel();
        let panel = library.netease.as_mut().unwrap();
        panel.download_job = Some(NeteaseDownloadJob {
            song: active.clone(),
            task: ManagedTask::from_receiver(result),
        });
        panel.download_queue.push_back(pending.clone());
        panel.songs.push(replacement.clone());
        panel.selected_ids.insert(replacement.id);
        panel.download_summary.completed = 1;

        handle_netease_source_key(&mut library, KeyCode::Enter).unwrap();

        assert!(library.netease.as_ref().unwrap().modal.is_none());
        assert_eq!(
            library.netease_message.as_deref(),
            Some("Wait for the current NetEase download queue to finish")
        );

        library.netease.as_mut().unwrap().modal = Some(NeteaseModal::ConfirmDownload);
        handle_netease_modal_key(&mut library, KeyCode::Char('y')).unwrap();

        let panel = library.netease.as_ref().unwrap();
        assert_eq!(panel.download_job.as_ref().unwrap().song, active);
        assert_eq!(panel.download_queue.front(), Some(&pending));
        assert_eq!(panel.download_summary.completed, 1);
    }

    #[test]
    fn source_switch_restores_each_features_own_status() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(music.join("local.wav"), b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        library.start_import();
        let (sender, result) = mpsc::channel();
        library.netease.as_mut().unwrap().catalog_job = Some(CatalogJob {
            task: ManagedTask::from_receiver(result),
        });
        sender
            .send(Ok(super::CatalogResult::Liked(Vec::new())))
            .unwrap();
        poll_netease_catalog(&mut library).unwrap();
        let backend = TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        library.music_source = super::MusicSource::Local;
        terminal
            .draw(|frame| draw_library_sources(frame, frame.area(), &library))
            .unwrap();
        let local = terminal.backend().buffer().content().to_vec();
        let local = local
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(local.contains("Creating project and separating"), "{local}");
        assert!(!local.contains("Loaded 0 liked songs"), "{local}");

        library.music_source = super::MusicSource::Netease;
        terminal
            .draw(|frame| draw_library_sources(frame, frame.area(), &library))
            .unwrap();
        let netease = terminal.backend().buffer().content().to_vec();
        let netease = netease
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(netease.contains("Loaded 0 liked songs"), "{netease}");
        assert!(
            !netease.contains("Creating project and separating"),
            "{netease}"
        );
    }

    #[test]
    fn final_netease_download_starts_project_creation_and_separation() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        let downloaded = music.join("NetEase/Artist-Song.flac");
        fs::create_dir_all(downloaded.parent().unwrap()).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(&downloaded, b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": "/bin/false",
                "log_dir": sandbox.path().join("logs")
            },
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let panel = library.netease.as_mut().unwrap();
        let song = Song {
            id: 7,
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        };
        let (sender, result) = mpsc::channel();
        panel.download_job = Some(NeteaseDownloadJob {
            song,
            task: ManagedTask::from_receiver(result),
        });
        sender
            .send(Ok(crate::netease::DownloadOutcome::Downloaded {
                path: downloaded.clone(),
                quality: Quality::Lossless,
            }))
            .unwrap();

        poll_netease_download(&mut library).unwrap();

        let job = library.job.take().expect("separation should start");
        assert_eq!(job.source, downloaded);
        assert!(
            library
                .message
                .as_deref()
                .unwrap()
                .contains("Creating project and separating")
        );
        assert!(
            job.result
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn active_separation_project_cannot_be_opened() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let source = music.join("active.wav");
        fs::write(&source, b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"},
            "netease": {"enabled": true}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let project_path =
            crate::library::source_entry_for_path(&library.config, &source).project_path;
        library
            .snapshot
            .projects
            .push(crate::library::ProjectEntry {
                path: project_path,
                title: "Active".into(),
            });
        let (_sender, result) = mpsc::channel();
        library.job = Some(ImportJob {
            source,
            result,
            cancellation: Arc::new(AtomicBool::new(false)),
            worker: None,
        });
        library.focus = super::LibraryFocus::Projects;
        let mut current = None;

        handle_library_key(&mut library, &mut current, KeyCode::Enter).unwrap();

        assert!(current.is_none());
        assert_eq!(
            library.message.as_deref(),
            Some("Wait for separation to finish before opening this project")
        );
    }

    #[test]
    fn completed_chrome_login_updates_status_without_exposing_the_cookie() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let session_store = SessionStore::at(sandbox.path().join("session.json"));
        fs::write(
            session_store.path(),
            r#"{"cookie":"MUSIC_U=must-not-appear","user_id":42,"nickname":"Singer"}"#,
        )
        .unwrap();
        let mut panel = NeteasePanel::new(
            session_store,
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        let (_catalog_sender, catalog_result) = mpsc::channel();
        panel.catalog_job = Some(super::CatalogJob {
            task: ManagedTask::from_receiver(catalog_result),
        });
        let (sender, result) = mpsc::channel();
        panel.chrome_login_job = Some(ChromeLoginJob {
            task: ManagedTask::from_receiver(result),
        });
        sender.send(Ok(())).unwrap();
        library.netease = Some(panel);

        poll_netease_chrome_login(&mut library).unwrap();

        assert!(
            library
                .netease
                .as_ref()
                .is_some_and(|panel| panel.chrome_login_job.is_none())
        );
        assert_eq!(
            library.netease_message.as_deref(),
            Some("Logged in to NetEase as Singer via Chrome")
        );
        assert!(
            !library
                .netease_message
                .as_deref()
                .unwrap()
                .contains("must-not-appear")
        );
    }

    #[test]
    fn expired_netease_session_returns_to_explicit_login_without_dropping_queue() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        fs::write(
            store.path(),
            r#"{"cookie":"MUSIC_U=expired","user_id":42,"nickname":"Singer"}"#,
        )
        .unwrap();
        let mut panel = NeteasePanel::new(
            store.clone(),
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        panel.download_queue.push_back(Song {
            id: 7,
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        });
        panel.modal = Some(NeteaseModal::Risk);

        panel.expire_session().unwrap();

        assert!(store.load().unwrap().is_none());
        assert!(panel.modal.is_none());
        assert_eq!(panel.download_queue.len(), 1);
        assert_eq!(
            super::netease_session_expired_hint(),
            "NetEase session expired · press c to import Chrome login · i for QR"
        );
    }

    #[test]
    fn expired_catalog_job_does_not_open_qr_or_exit_the_tui() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        fs::write(
            store.path(),
            r#"{"cookie":"MUSIC_U=expired","user_id":42,"nickname":"Singer"}"#,
        )
        .unwrap();
        let mut panel = NeteasePanel::new(
            store.clone(),
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        let (sender, result) = mpsc::channel();
        panel.catalog_job = Some(CatalogJob {
            task: ManagedTask::from_receiver(result),
        });
        sender.send(Err(NeteaseError::LoginRequired)).unwrap();
        library.netease = Some(panel);

        poll_netease_catalog(&mut library).unwrap();

        assert!(store.load().unwrap().is_none());
        assert!(library.netease.as_ref().unwrap().modal.is_none());
        assert_eq!(
            library.netease_message.as_deref(),
            Some("NetEase session expired · press c to import Chrome login · i for QR")
        );
    }

    #[test]
    fn expired_download_job_keeps_the_song_queued_for_the_next_login() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let store = SessionStore::at(sandbox.path().join("session.json"));
        fs::write(
            store.path(),
            r#"{"cookie":"MUSIC_U=expired","user_id":42,"nickname":"Singer"}"#,
        )
        .unwrap();
        let mut panel = NeteasePanel::new(
            store.clone(),
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        let pending = Song {
            id: 7,
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        };
        let (sender, result) = mpsc::channel();
        panel.download_job = Some(NeteaseDownloadJob {
            song: pending.clone(),
            task: ManagedTask::from_receiver(result),
        });
        sender.send(Err(NeteaseError::LoginRequired)).unwrap();
        library.netease = Some(panel);

        poll_netease_download(&mut library).unwrap();

        let panel = library.netease.as_ref().unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(panel.modal.is_none());
        assert_eq!(panel.download_queue.front(), Some(&pending));
        assert_eq!(
            library.netease_message.as_deref(),
            Some("NetEase session expired · press c to import Chrome login · i for QR")
        );
    }

    #[test]
    fn missing_configured_lyrics_are_treated_as_not_loaded() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("song.wav");
        let lyrics = sandbox.path().join("song.lrc");
        fs::write(&song, b"audio").unwrap();
        fs::write(&lyrics, b"[00:01.00]line").unwrap();
        let project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: Some(lyrics),
                title: None,
            })
            .unwrap();
        let configured_lyrics = project.lyrics().unwrap();
        fs::remove_file(project.root().join(configured_lyrics.as_str())).unwrap();

        assert!(load_lyrics(&project).unwrap().is_none());
    }

    #[test]
    fn selected_online_lyrics_are_saved_only_after_confirmation() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("同名歌曲.flac");
        fs::write(&song, b"audio").unwrap();
        let project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("同名歌曲".into()),
            })
            .unwrap();
        let mut app = App::new(project, None, None, VocalEffectPreset::Clean);
        app.lyrics_picker = Some(LyricsPicker {
            choices: vec![
                LyricsChoice::for_test("test", "同名歌曲", "歌手甲", 180.0, "[00:01.00]错误版本"),
                LyricsChoice::for_test("test", "同名歌曲", "歌手乙", 182.0, "[00:01.00]正确版本"),
            ],
            selected: 0,
        });

        assert!(app.session.project().lyrics().is_none());
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Enter);

        let relative = app.session.project().lyrics().unwrap();
        let saved =
            fs::read_to_string(app.session.project().root().join(relative.as_str())).unwrap();
        assert!(saved.contains("正确版本"), "{saved}");
        assert!(!saved.contains("错误版本"), "{saved}");
        assert!(app.lyrics_picker.is_none());
        assert_eq!(app.lyrics_origin.as_deref(), Some("test · auto"));
        assert!(
            app.recording_message
                .as_deref()
                .is_some_and(|message| message.contains("test · auto"))
        );
    }

    #[test]
    fn lyrics_search_editor_edits_unicode() {
        let mut editor = LyricsQueryEditor::new("难舍难分".into());

        editor.backspace();
        editor.insert('份');
        editor.cursor = 0;
        editor.delete();
        editor.insert('难');

        assert_eq!(editor.query, "难舍难份");
        assert_eq!(editor.cursor, 1);
    }

    #[test]
    fn lyrics_search_editor_opens_with_default_title() {
        let sandbox = tempfile::tempdir().unwrap();
        let song = sandbox.path().join("source.flac");
        fs::write(&song, b"audio").unwrap();
        let project = FileProjectRepository
            .create(CreateProject {
                root: sandbox.path().join("project"),
                song,
                lyrics: None,
                title: Some("默认搜索名".into()),
            })
            .unwrap();
        let mut app = App::new(project, None, None, VocalEffectPreset::Clean);

        app.handle_key(KeyCode::Char('l'));

        assert_eq!(
            app.lyrics_query_editor
                .as_ref()
                .map(|editor| editor.query.as_str()),
            Some("默认搜索名")
        );
    }

    #[test]
    fn selected_lyrics_candidate_renders_preview_and_origin() {
        let picker = LyricsPicker {
            choices: vec![
                LyricsChoice::for_test(
                    "LRCLIB",
                    "同名歌曲",
                    "歌手甲",
                    180.0,
                    "[00:01.00]错误歌词预览",
                ),
                LyricsChoice::for_test(
                    "LRCLIB",
                    "同名歌曲",
                    "歌手乙",
                    182.0,
                    "[00:01.00]正确歌词预览\n[00:05.00]下一句",
                ),
            ],
            selected: 1,
        };
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| super::draw_lyrics_picker(frame, frame.area(), &picker))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let compact = rendered.replace(' ', "");
        assert!(compact.contains("正确歌词预览"), "{rendered}");
        assert!(compact.contains("下一句"), "{rendered}");
        assert!(rendered.contains("LRCLIB · auto"), "{rendered}");
    }

    #[test]
    fn imports_can_be_queued_while_another_source_is_running() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(music.join("a.wav"), b"a").unwrap();
        fs::write(music.join("b.wav"), b"b").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": "/bin/false",
                "log_dir": sandbox.path().join("logs")
            }
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let active_source = library.snapshot.sources[0].path.clone();
        let queued_source = library.snapshot.sources[1].path.clone();
        library.snapshot.sources[1].project_state = SourceProjectState::Current;
        let (sender, receiver) = mpsc::channel();
        library.job = Some(ImportJob {
            source: active_source,
            result: receiver,
            cancellation: Arc::new(AtomicBool::new(false)),
            worker: None,
        });

        library.source_selected = 1;
        library.start_import();

        assert_eq!(
            library
                .queue
                .iter()
                .map(|request| &request.source.path)
                .collect::<Vec<_>>(),
            [&queued_source]
        );
        assert_eq!(
            library.queue[0].source.project_state,
            SourceProjectState::Current
        );
        assert!(library.message.as_deref().unwrap().contains("Queued at"));
        library.start_import();
        assert_eq!(library.queue.len(), 1);
        assert!(
            library
                .message
                .as_deref()
                .unwrap()
                .contains("Already queued")
        );

        sender.send(Err("simulated failure".into())).unwrap();
        poll_import_job(&mut library).unwrap();

        assert_eq!(library.job.as_ref().unwrap().source, queued_source);
        assert!(library.queue.is_empty());
        assert!(
            library
                .message
                .as_deref()
                .unwrap()
                .contains("Queue continues")
        );
    }

    #[test]
    fn source_s_key_queues_reseparation_for_an_existing_project() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let song = music.join("song.wav");
        fs::write(&song, b"song").unwrap();
        FileProjectRepository
            .create(CreateProject {
                root: projects.join("song"),
                song,
                lyrics: None,
                title: None,
            })
            .unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let (_sender, receiver) = mpsc::channel();
        library.job = Some(ImportJob {
            source: sandbox.path().join("active.wav"),
            result: receiver,
            cancellation: Arc::new(AtomicBool::new(false)),
            worker: None,
        });

        handle_library_source_key(&mut library, &mut None, KeyCode::Char('s')).unwrap();

        assert_eq!(library.queue.len(), 1);
        assert_eq!(
            library.queue[0].source.project_state,
            SourceProjectState::Current
        );
        assert!(library.message.as_deref().unwrap().contains("Queued at"));
    }

    #[test]
    fn completed_import_does_not_open_or_autoplay_the_project() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(music.join("song.wav"), b"song").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {
                "worker": "/bin/false",
                "log_dir": sandbox.path().join("logs")
            }
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let completed = projects.join("song");
        let (sender, receiver) = mpsc::channel();
        library.job = Some(ImportJob {
            source: library.snapshot.sources[0].path.clone(),
            result: receiver,
            cancellation: Arc::new(AtomicBool::new(false)),
            worker: None,
        });
        sender.send(Ok(completed)).unwrap();
        poll_import_job(&mut library).unwrap();

        assert!(
            library
                .message
                .as_deref()
                .unwrap()
                .starts_with("Separation complete")
        );
    }

    #[test]
    fn quit_during_separation_requires_confirmation() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let source = music.join("song.wav");
        fs::write(&source, b"song").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let (_sender, receiver) = mpsc::channel();
        library.job = Some(ImportJob {
            source,
            result: receiver,
            cancellation: cancellation.clone(),
            worker: None,
        });

        let should_exit = handle_library_key(&mut library, &mut None, KeyCode::Char('q')).unwrap();

        assert!(!should_exit);
        assert!(library.confirm_exit);
        assert!(!cancellation.load(Ordering::Acquire));
        assert!(library.job.is_some());
    }

    #[test]
    fn confirmed_quit_cancels_active_separation_and_clears_queue() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        fs::write(music.join("active.wav"), b"active").unwrap();
        fs::write(music.join("queued.wav"), b"queued").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while !worker_cancellation.load(Ordering::Acquire) {
                thread::yield_now();
            }
            let _ = sender.send(Err("separation cancelled".into()));
        });
        library.job = Some(ImportJob {
            source: library.snapshot.sources[0].path.clone(),
            result: receiver,
            cancellation: cancellation.clone(),
            worker: Some(worker),
        });
        library.queue.push_back(SeparationRequest {
            source: library.snapshot.sources[1].clone(),
        });
        let remote_cancelled = Arc::new(AtomicBool::new(false));
        let observed_cancellation = remote_cancelled.clone();
        let task = ManagedTask::spawn(move |cancelled| {
            while !cancelled.load(Ordering::Acquire) {
                thread::yield_now();
            }
            observed_cancellation.store(true, Ordering::Release);
            Err(NeteaseError::Cancelled)
        });
        let mut panel = NeteasePanel::new(
            SessionStore::at(sandbox.path().join("session.json")),
            RiskStore::at(sandbox.path().join("preferences.json")),
        )
        .unwrap();
        panel.download_job = Some(NeteaseDownloadJob {
            song: Song {
                id: 7,
                title: "Remote".into(),
                artists: vec!["Artist".into()],
                album: "Album".into(),
                cover_url: None,
                max_quality: Quality::Lossless,
                available: true,
            },
            task,
        });
        panel.download_queue.push_back(Song {
            id: 8,
            title: "Queued".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            cover_url: None,
            max_quality: Quality::Lossless,
            available: true,
        });
        library.netease = Some(panel);

        assert!(!handle_library_key(&mut library, &mut None, KeyCode::Char('q')).unwrap());
        assert!(handle_library_key(&mut library, &mut None, KeyCode::Char('y')).unwrap());

        assert!(cancellation.load(Ordering::Acquire));
        assert!(library.job.is_none());
        assert!(library.queue.is_empty());
        assert!(remote_cancelled.load(Ordering::Acquire));
        let panel = library.netease.as_ref().unwrap();
        assert!(panel.download_job.is_none());
        assert!(panel.download_queue.is_empty());
    }

    #[test]
    fn wrapped_source_rows_keep_the_selected_item_visible() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        for name in [
            "first-very-long-audio-filename.wav",
            "second-very-long-audio-filename.wav",
            "selected-item.wav",
        ] {
            fs::write(music.join(name), b"audio").unwrap();
        }
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        library.focus = super::LibraryFocus::Sources;
        library.source_selected = 2;
        let backend = TestBackend::new(24, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| super::draw_library_sources(frame, frame.area(), &library))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("selected-item"), "{rendered}");
        let selected_is_highlighted = (1..buffer.area.height.saturating_sub(1)).any(|y| {
            (1..buffer.area.width.saturating_sub(1)).any(|x| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.symbol() == "s" && cell.fg == Color::Yellow)
            })
        });
        assert!(selected_is_highlighted, "{rendered}");
    }

    #[test]
    fn long_source_name_is_shortened_to_one_row_and_keeps_its_extension() {
        let fitted = fit_source_name("some looooooooooong name.flac", 18);

        assert!(fitted.chars().count() <= 18, "{fitted}");
        assert!(fitted.contains('…'), "{fitted}");
        assert!(
            Path::new(&fitted)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("flac")),
            "{fitted}"
        );
        assert!(!fitted.contains('\n'));
    }

    #[test]
    fn selected_long_source_name_expands_in_place() {
        let sandbox = tempfile::tempdir().unwrap();
        let music = sandbox.path().join("music");
        let projects = sandbox.path().join("projects");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&projects).unwrap();
        let filename = "selected-super-long-audio-filename.wav";
        fs::write(music.join(filename), b"audio").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "music_root": music,
            "projects_root": projects,
            "separation": {"worker": "/bin/false"}
        }))
        .unwrap();
        let mut library = MediaLibrary::new(config).unwrap();
        library.focus = super::LibraryFocus::Sources;
        let backend = TestBackend::new(24, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| super::draw_library_sources(frame, frame.area(), &library))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let highlighted = (1..buffer.area.height.saturating_sub(1))
            .flat_map(|y| {
                (1..buffer.area.width.saturating_sub(1)).filter_map(move |x| {
                    buffer.cell((x, y)).and_then(|cell| {
                        (cell.fg == Color::Yellow && !cell.symbol().trim().is_empty())
                            .then(|| cell.symbol().to_owned())
                    })
                })
            })
            .collect::<String>();
        assert!(highlighted.contains(filename), "{highlighted}");
    }

    #[test]
    fn formats_playback_position_as_minutes_and_seconds() {
        assert_eq!(format_duration(Duration::from_secs(125)), "02:05");
    }

    #[test]
    fn footer_disables_actions_that_the_recording_mode_rejects() {
        assert!(mode_allows_footer_action(
            RecordingState::Idle,
            FooterAction::Arm
        ));
        assert!(!mode_allows_footer_action(
            RecordingState::Idle,
            FooterAction::RecordToggle
        ));
        assert!(mode_allows_footer_action(
            RecordingState::Armed,
            FooterAction::RecordToggle
        ));
        assert!(!mode_allows_footer_action(
            RecordingState::Armed,
            FooterAction::Effect
        ));
        assert!(mode_allows_footer_action(
            RecordingState::Idle,
            FooterAction::Lyrics
        ));
        assert!(!mode_allows_footer_action(
            RecordingState::Recording,
            FooterAction::Lyrics
        ));
        assert!(!mode_allows_footer_action(
            RecordingState::Recording,
            FooterAction::Playback
        ));
        assert!(mode_allows_footer_action(
            RecordingState::Recording,
            FooterAction::Volume
        ));
        assert!(mode_allows_footer_action(
            RecordingState::Recording,
            FooterAction::SourceTrack
        ));
    }

    #[test]
    fn effect_chord_selects_a_preset_without_cycling() {
        assert_eq!(
            effect_preset_for_key(KeyCode::Char('1')),
            Some(VocalEffectPreset::Clean)
        );
        assert_eq!(
            effect_preset_for_key(KeyCode::Char('3')),
            Some(VocalEffectPreset::Ktv)
        );
        assert_eq!(
            effect_preset_for_key(KeyCode::Char('5')),
            Some(VocalEffectPreset::Church)
        );
        assert_eq!(effect_preset_for_key(KeyCode::Char('6')), None);
    }

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
    fn lyric_countdown_shows_at_most_three_seconds_for_the_next_line() {
        let timeline = LyricsTimeline::parse("[00:02.000]第一句\n[00:07.000]第二句");

        assert_eq!(
            lyric_countdown(&timeline, Duration::ZERO),
            Some(LyricCountdown {
                index: 0,
                seconds: 2
            })
        );
        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(500)),
            Some(LyricCountdown {
                index: 0,
                seconds: 2
            })
        );
        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(4_001)),
            Some(LyricCountdown {
                index: 1,
                seconds: 3
            })
        );
        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(6_001)),
            Some(LyricCountdown {
                index: 1,
                seconds: 1
            })
        );
        assert_eq!(lyric_countdown(&timeline, Duration::from_secs(7)), None);
    }

    #[test]
    fn lyric_countdown_ignores_intervals_shorter_than_one_second() {
        let timeline = LyricsTimeline::parse("[00:02.000]快\n[00:02.900]歌词");

        assert_eq!(
            lyric_countdown(&timeline, Duration::from_millis(2_100)),
            None
        );
    }

    #[test]
    fn recording_arrows_seek_to_previous_and_next_lyric_timestamps() {
        let timeline =
            LyricsTimeline::parse("[00:01.000]第一句\n[00:04.000]第二句\n[00:08.000]第三句");

        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(5), -1),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(5), 1),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_millis(500), 1),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(1), -1),
            None
        );
        assert_eq!(
            lyric_seek_target(&timeline, Duration::from_secs(9), 1),
            None
        );
    }

    #[test]
    fn recording_shortcuts_allow_switching_between_the_three_source_tracks() {
        assert_eq!(
            recording_track_for_key(KeyCode::Char('1')),
            Some(SessionTrackKind::Original)
        );
        assert_eq!(
            recording_track_for_key(KeyCode::Char('2')),
            Some(SessionTrackKind::Accompaniment)
        );
        assert_eq!(
            recording_track_for_key(KeyCode::Char('3')),
            Some(SessionTrackKind::Vocals)
        );
        assert_eq!(recording_track_for_key(KeyCode::Char('4')), None);
    }

    #[test]
    fn playback_keys_use_the_shared_application_commands() {
        assert!(matches!(
            playback_command_for_key(KeyCode::Char(' ')),
            Some(PlaybackCommand::Toggle)
        ));
        assert!(matches!(
            playback_command_for_key(KeyCode::Left),
            Some(PlaybackCommand::SeekBy(-5))
        ));
        assert!(matches!(
            playback_command_for_key(KeyCode::Char('2')),
            Some(PlaybackCommand::SwitchTrack(SharedTrackKind::Accompaniment))
        ));
    }

    #[test]
    fn session_playback_resolves_the_recording_accompaniment() {
        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path().join("project");
        for directory in ["source", "stems", "takes", "lyrics", "exports"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("source/song.wav"), b"audio").unwrap();
        fs::write(root.join("stems/accompaniment.wav"), b"audio").unwrap();
        fs::write(root.join("stems/vocals.wav"), b"audio").unwrap();
        fs::write(
            root.join("project.json"),
            r#"{
              "schema_version": 1,
              "id": "135a282d-5915-4b7f-a8da-1c5beff3eee3",
              "title": "Tracks",
              "source": "source/song.wav",
              "lyrics": null,
              "separation": {
                "status": "ready",
                "details": {
                  "vocals": "stems/vocals.wav",
                  "accompaniment": "stems/accompaniment.wav",
                  "provenance": {
                    "provider": "test",
                    "architecture": "test",
                    "checkpoint_id": "test",
                    "checkpoint_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "profile": "quality"
                  }
                }
              },
              "takes": [],
              "latency_compensation_ms": 0,
              "key_shift_semitones": 0,
              "effects_schema_version": 1
            }"#,
        )
        .unwrap();
        let project = FileProjectRepository.open(&root).unwrap();
        let playback = SessionPlayback::open(&project);

        assert_eq!(
            playback.accompaniment_path().unwrap(),
            root.join("stems/accompaniment.wav")
        );
    }

    #[test]
    fn ignores_windows_key_release_events() {
        let release = KeyEvent {
            code: KeyCode::Char('m'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };

        assert!(!should_handle_key(&release));
    }
}
