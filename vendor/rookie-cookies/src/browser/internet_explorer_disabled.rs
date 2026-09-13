use crate::common::enums::Cookie;
use anyhow::{bail, Result};
use std::path::PathBuf;

const FEATURE_DISABLED: &str =
  "Internet Explorer cookie extraction is not enabled in this build";

pub fn internet_explorer_based(
  _db_path: PathBuf,
  _domains: Option<Vec<String>>,
  _force_kill: bool,
) -> Result<Vec<Cookie>> {
  bail!(FEATURE_DISABLED)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InternetExplorerExtractionStats {
  pub(crate) records_seen: usize,
  pub(crate) records_skipped: usize,
}

#[derive(Debug)]
pub(crate) struct InternetExplorerExtraction {
  pub(crate) cookies: Vec<Cookie>,
  pub(crate) stats: InternetExplorerExtractionStats,
  pub(crate) row_error: Option<String>,
}

pub(crate) fn internet_explorer_outcome(
  _db_path: PathBuf,
  _domains: Option<Vec<String>>,
  _force_kill: bool,
) -> Result<InternetExplorerExtraction> {
  bail!(FEATURE_DISABLED)
}
