//! GUI message translations shared by Rust status producers and Slint catalogs.

use std::{collections::HashMap, sync::OnceLock};

use crate::settings::GuiLanguage;

const ZH_HANS: &str = include_str!("../translations/zh-Hans/LC_MESSAGES/k3-gui.po");
const ZH_HANT: &str = include_str!("../translations/zh-Hant/LC_MESSAGES/k3-gui.po");

static HANS_CATALOG: OnceLock<HashMap<String, String>> = OnceLock::new();
static HANT_CATALOG: OnceLock<HashMap<String, String>> = OnceLock::new();

// These messages contain app-owned variables. Paths, file names, device names, and
// external errors remain verbatim; only the surrounding K3 text is translated.
const TEMPLATES: &[&str] = &[
    "Searching {source}: {artist} - {title}...",
    "Searching {source}: {title}...",
    "No {source} artist match; retrying by title only: {title}...",
    "{source} request failed: {reason}; retrying (2/2)...",
    "Found synced lyrics on {source}: {artist} - {track} ({duration_seconds}s)",
    "Saving lyrics: {relative_path}",
    "Projects folder is not readable: {path}",
    "Searching online lyrics: {query}",
    "Found {count} synced lyric versions",
    "Lyrics search failed: {error}",
    "Saved lyrics: {artist} - {track} · {origin}",
    "Cannot save synchronized lyrics: {error}",
    "Queued {count} audio file(s)",
    "{count} files: {names}",
    "{name}: another selected file has the same project name",
    "{name}: destination changed; select the file again",
    "{name}: already queued or separating",
    "Separating {name}. This may take several minutes…",
    "Separated {count} song(s); {failed} failed{details}",
    ". Last error: {error}",
    "Cannot render take: {error}",
    "{effect} saved · playing",
    "Recording · {device}",
    "Cannot start recording: {error}",
    "Cannot save recording: {error}",
    "Saved {duration}s take from {device}",
    "Take {index} of {count} · latest",
    "Take {index} of {count}",
    "NetEase setup failed: {error}",
    "NetEase session failed: {error}",
    "Cannot enable NetEase: {error}",
    "Chrome login failed: {error}",
    "QR login failed: {error}",
    "Login failed: {error}",
    "Log out failed: {error}",
    "Already queued: {title}",
    "Queued {count} songs for download and separation",
    "Downloads finished: {count} processed, {issues} issues{details}",
    "Downloading {title} · {waiting} waiting…",
    "Retrying separation for {title} · {waiting} waiting",
    "Downloaded, but not separated: {issue}",
    "Downloaded {title} · {waiting} waiting to separate",
    "Downloaded {title} but cannot prepare project: {error}",
    "NetEase download failed for {title}: {error}",
    "Logged in to NetEase as {name}",
    "Found {count} songs; select any number to queue{cache_warning}",
    "; downloaded status unavailable: {error}",
    " · Warning: {warning}",
    "NetEase catalog failed: {error}",
    "Cannot render login QR code: {error}",
    "Audio file is not readable: {path}",
    "Destination already exists but is not a K3 project: {path}",
    "Separation script is missing: {path}",
    "Could not start the separation script: {error}",
    "{name}: {error}",
];

pub(crate) fn message(source: &str, language: GuiLanguage) -> String {
    if language == GuiLanguage::English || source.is_empty() {
        return source.to_owned();
    }
    let catalog = match language {
        GuiLanguage::English => unreachable!(),
        GuiLanguage::SimplifiedChinese => HANS_CATALOG.get_or_init(|| parse_catalog(ZH_HANS)),
        GuiLanguage::TraditionalChinese => HANT_CATALOG.get_or_init(|| parse_catalog(ZH_HANT)),
    };
    source
        .split('\n')
        .map(|line| translate_line(line, catalog))
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_line(source: &str, catalog: &HashMap<String, String>) -> String {
    if let Some(translated) = catalog.get(source) {
        return translated.clone();
    }
    if let Some((saved, warning)) = source.split_once(" · Warning: ")
        && saved.starts_with("Saved ")
    {
        return format!(
            "{}{}",
            translate_line(saved, catalog),
            translate_line(&format!(" · Warning: {warning}"), catalog)
        );
    }
    for template in TEMPLATES {
        if let Some(captures) = capture_template(template, source)
            && let Some(translated) = catalog.get(*template)
        {
            return fill_template(translated, &captures, catalog);
        }
    }
    source.to_owned()
}

fn capture_template<'a>(template: &str, source: &'a str) -> Option<HashMap<String, &'a str>> {
    let mut captures = HashMap::new();
    let mut pattern = template;
    let mut input = source;
    loop {
        let Some(open) = pattern.find('{') else {
            return (pattern == input).then_some(captures);
        };
        let literal = &pattern[..open];
        input = input.strip_prefix(literal)?;
        let close = pattern[open + 1..].find('}')? + open + 1;
        let key = &pattern[open + 1..close];
        pattern = &pattern[close + 1..];
        let next_literal = pattern.split('{').next().unwrap_or("");
        if next_literal.is_empty() && pattern.contains('{') {
            return None; // Adjacent placeholders cannot be split safely.
        }
        let end = if next_literal.is_empty() {
            input.len()
        } else {
            input.find(next_literal)?
        };
        captures.insert(key.to_owned(), &input[..end]);
        input = &input[end..];
    }
}

fn fill_template(
    template: &str,
    captures: &HashMap<String, &str>,
    catalog: &HashMap<String, String>,
) -> String {
    let mut result = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let Some(close) = rest[open + 1..].find('}') else {
            result.push_str(&rest[open..]);
            return result;
        };
        let key = &rest[open + 1..open + 1 + close];
        if let Some(value) = captures.get(key) {
            if key == "effect" {
                result.push_str(catalog.get(*value).map_or(*value, String::as_str));
            } else if key == "cache_warning"
                || key == "details"
                || (key == "error" && captures.contains_key("name"))
            {
                result.push_str(&translate_line(value, catalog));
            } else {
                result.push_str(value);
            }
        } else {
            result.push_str(&rest[open..open + close + 2]);
        }
        rest = &rest[open + close + 2..];
    }
    result.push_str(rest);
    result
}

fn parse_catalog(source: &str) -> HashMap<String, String> {
    let mut catalog = HashMap::new();
    let mut id = String::new();
    let mut value = String::new();
    let mut in_value = false;
    for line in source.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !id.is_empty() && !value.is_empty() {
                catalog.insert(std::mem::take(&mut id), std::mem::take(&mut value));
            }
            id.clear();
            value.clear();
            in_value = false;
        } else if let Some(fragment) = line.strip_prefix("msgid ") {
            id = serde_json::from_str(fragment).unwrap_or_default();
            in_value = false;
        } else if let Some(fragment) = line.strip_prefix("msgstr ") {
            value = serde_json::from_str(fragment).unwrap_or_default();
            in_value = true;
        } else if line.starts_with('"')
            && let Ok(fragment) = serde_json::from_str::<String>(line)
        {
            if in_value {
                value.push_str(&fragment);
            } else {
                id.push_str(&fragment);
            }
        }
    }
    catalog
}

#[cfg(test)]
mod tests {
    use super::{capture_template, message, parse_catalog};
    use crate::settings::GuiLanguage;

    #[test]
    fn catalogs_have_every_runtime_template() {
        for catalog in [super::ZH_HANS, super::ZH_HANT] {
            let parsed = parse_catalog(catalog);
            for template in super::TEMPLATES {
                assert!(parsed.contains_key(*template), "missing {template}");
            }
            for line in include_str!("../ui/strings.slint").lines() {
                if let Some((_, source)) = line.split_once("@tr(") {
                    let source = source.strip_suffix(");").unwrap().trim();
                    let source: String = serde_json::from_str(source).unwrap();
                    assert!(parsed.contains_key(&source), "missing {source}");
                }
            }
        }
    }

    #[test]
    fn translates_status_with_external_details_unchanged() {
        assert_eq!(
            message(
                "Searching online lyrics: 测试",
                GuiLanguage::TraditionalChinese
            ),
            "正在搜尋線上歌詞：测试"
        );
        assert_eq!(
            message(
                "The exact file C:\\song.wav",
                GuiLanguage::SimplifiedChinese
            ),
            "The exact file C:\\song.wav"
        );
        assert!(
            capture_template(
                "Saved {duration}s take from {device}",
                "Saved 2.5s take from Mic"
            )
            .is_some()
        );
        assert_eq!(
            message(
                "Saved 2.5s take from Mic · Warning: clipped",
                GuiLanguage::SimplifiedChinese
            ),
            "已保存来自 Mic 的 2.5 秒录音 · 警告：clipped"
        );
        assert_eq!(
            message("church saved · playing", GuiLanguage::SimplifiedChinese),
            "已保存 教堂 效果 · 正在播放"
        );
    }
}
