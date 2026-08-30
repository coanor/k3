pub(crate) mod chromium;
pub(crate) mod chromium_crypto;
pub(crate) mod chromium_platform_keys;
#[cfg(any(
  all(target_os = "windows", feature = "internet-explorer"),
  test
))]
pub(crate) mod internet_explorer_model;
pub(crate) mod mozilla;
pub(crate) mod registry;
pub(crate) mod report_build;
pub(crate) mod report_core;

#[cfg(all(target_os = "windows", feature = "internet-explorer"))]
pub(crate) mod internet_explorer;

#[cfg(all(target_os = "windows", not(feature = "internet-explorer")))]
#[path = "internet_explorer_disabled.rs"]
pub(crate) mod internet_explorer;

#[cfg(any(target_os = "macos", test))]
pub(crate) mod safari;
