use std::io;

#[derive(Debug, thiserror::Error)]
pub enum NeteaseError {
    #[error("local NetEase data error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid local NetEase data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("NetEase request failed: {0}")]
    Http(String),
    #[error("NetEase protocol error: {0}")]
    Protocol(String),
    #[error("NetEase login is required")]
    LoginRequired,
    #[error("cannot import NetEase login from Chrome: {0}")]
    ChromeLogin(String),
    #[error("cannot write NetEase audio metadata: {0}")]
    Metadata(String),
    #[error("NetEase download was cancelled")]
    Cancelled,
}

mod auth;
mod client;
mod download;
mod provider;
mod weapi;

pub use auth::{LoginStatus, NeteaseSession, RiskStore, SessionStore, local_stores};
pub use client::NeteaseClient;
pub use download::{DownloadOutcome, Quality, Song, SongPage};

use auth::{
    AccountProfile, AuthorizedLogin, ChromeCookieSource, LoginPoll, LoginTicket,
    RookieChromeCookieSource, chrome_cookie_header, write_private_json,
};
use download::{
    AudioSource, DownloadDecision, DownloadIndex, DownloadPaths, commit_download, retry_transient,
    tag_audio, write_bytes,
};
use provider::{NeteaseProvider, WebNeteaseProvider};

#[cfg(test)]
use auth::{BrowserCookie, ChromeCookieProfile};
#[cfg(test)]
use download::{DownloadRecord, download_filename};
#[cfg(test)]
use provider::{NETEASE_BASE_URL, NETEASE_USER_AGENT, weapi_cookie_header};

#[cfg(test)]
mod tests;
