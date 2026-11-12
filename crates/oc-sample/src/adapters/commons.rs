//! Wikimedia Commons: a MediaWiki search API with no key, direct file URLs and
//! a licence on every file.
//!
//! Commons is an archive, not a sample pack. It has the recordings, the
//! durations and the licences; a "drum one-shot" is only as clean as the file
//! an uploader happened to contribute. That is why the default search is text
//! plus a kind hint and the licence rule refuses anything that is not CC0,
//! public domain, CC BY or CC BY-SA.
//!
//! Search asks for a wider page than the caller wants and applies the duration
//! and channel bounds locally: the MediaWiki search index has no duration
//! field, so a server-side filter would silently drop matches.

use serde_json::Value;

use crate::license::License;
use crate::model::{Quality, SampleError, SampleItem, SamplePage, SampleQuery, classify};
use crate::provider::{SampleFetch, SampleProvider, encode, get_json};
use crate::providers::commons::{ID_PREFIX, PROVIDER_ID};

const API: &str = "https://commons.wikimedia.org/w/api.php";
/// Extra results fetched so local filtering still fills one page.
const OVERFETCH: usize = 3;
/// Containers the engine can decode as an audio clip. Commons files MIDI
/// sequences and video under the same `filetype:audio` search, and neither is a
/// sample a DAW can place.
const SAMPLE_EXTENSIONS: [&str; 7] = ["wav", "flac", "mp3", "ogg", "oga", "aif", "aiff"];
const MAX_FETCH: usize = 100;
const CATEGORY_LIMIT: usize = 20;

/// Adapter for one external sample market.
pub struct CommonsProvider;

impl CommonsProvider {
    /// Create the provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CommonsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleProvider for CommonsProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn label(&self) -> &'static str {
        "Wikimedia Commons"
    }

    fn search(
        &self,
        query: &SampleQuery,
        fetch: &dyn SampleFetch,
    ) -> Result<SamplePage, SampleError> {
        let body = get_json(fetch, "Wikimedia Commons", &search_url(query))?;
        let pages = body
            .pointer("/query/pages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        // Keep each match's position in the raw page: the next offset counts
        // raw results, so it must not skip the ones this page filtered out.
        let mut matched: Vec<(usize, SampleItem)> = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let Some(item) = item_from_page(page) else {
                continue;
            };
            if query.matches(&item) {
                matched.push((index, item));
            }
        }
        let raw = pages.len();
        let last = matched
            .iter()
            .take(query.page_size())
            .map(|(index, _)| *index)
            .next_back();
        let items: Vec<SampleItem> = matched
            .into_iter()
            .take(query.page_size())
            .map(|(_, item)| item)
            .collect();
        let continuation = body
            .pointer("/continue/gsroffset")
            .and_then(Value::as_u64)
            .map(|offset| offset as usize);
        let next_offset = match last {
            // A page can carry more raw results than the caller asked for;
            // follow the last returned index even without a continuation.
            Some(index) if index + 1 < raw || continuation.is_some() => {
                Some(query.offset + index + 1)
            }
            Some(_) => None,
            None => continuation.map(|page_end| page_end.max(query.offset + raw)),
        };
        Ok(SamplePage {
            provider: PROVIDER_ID.to_string(),
            offset: query.offset,
            limit: query.page_size(),
            total: None,
            next_offset,
            items,
        })
    }

    fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError> {
        let title = id.strip_prefix(ID_PREFIX).ok_or_else(|| {
            SampleError::refused(
                "unknown_id",
                format!(
                    "'{id}' is not a Wikimedia Commons id. Search first and pass the id from the result, for example commons:File:Snare drum unmuffled.ogg."
                ),
            )
        })?;
        let body = get_json(fetch, "Wikimedia Commons", &file_url(title))?;
        let wanted = normalize_title(title);
        let pages = body
            .pointer("/query/pages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let page = pages
            .iter()
            .find(|page| {
                page.get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|found| normalize_title(found) == wanted)
            })
            .or_else(|| {
                if pages.len() == 1 {
                    pages.first()
                } else {
                    None
                }
            });
        match page.and_then(item_from_page) {
            Some(item) => Ok(item),
            None => Err(SampleError::refused(
                "unknown_id",
                format!(
                    "Wikimedia Commons has no audio file '{title}'; it may have been deleted or renamed."
                ),
            )),
        }
    }
}

fn search_url(query: &SampleQuery) -> String {
    let limit = query.page_size();
    let fetch_limit = (limit * OVERFETCH).clamp(limit, MAX_FETCH);
    let mut terms: Vec<String> = vec!["filetype:audio".to_string()];
    if let Some(kind) = query.kind {
        for term in kind.search_terms() {
            terms.push((*term).to_string());
        }
    }
    let text = query.text.trim();
    if !text.is_empty() {
        terms.push(text.to_string());
    }
    for tag in &query.tags {
        let tag = tag.trim();
        if !tag.is_empty() {
            terms.push(format!("\"{tag}\""));
        }
    }
    format!(
        "{API}?action=query&format=json&formatversion=2&generator=search&gsrnamespace=6&gsrlimit={fetch_limit}&gsroffset={offset}&gsrsearch={search}&prop=imageinfo%7Ccategories&iiprop=url%7Csize%7Cmime%7Cmediatype%7Cextmetadata%7Cmetadata&iiextmetadatafilter=LicenseShortName%7CLicenseUrl%7CArtist%7CUsageTerms&cllimit={CATEGORY_LIMIT}&clshow=%21hidden",
        offset = query.offset,
        search = encode(&terms.join(" ")),
    )
}

fn file_url(title: &str) -> String {
    format!(
        "{API}?action=query&format=json&formatversion=2&titles={title}&prop=imageinfo%7Ccategories&iiprop=url%7Csize%7Cmime%7Cmediatype%7Cextmetadata%7Cmetadata&iiextmetadatafilter=LicenseShortName%7CLicenseUrl%7CArtist%7CUsageTerms&cllimit={CATEGORY_LIMIT}&clshow=%21hidden",
        title = encode(title),
    )
}

fn item_from_page(page: &Value) -> Option<SampleItem> {
    let title = page.get("title")?.as_str()?;
    let info = page.get("imageinfo")?.as_array()?.first()?;
    let download_url = strip_query(info.get("url")?.as_str()?);
    if download_url.is_empty() || !is_sample_file(&download_url) {
        return None;
    }
    let source_url = info.get("descriptionurl")?.as_str()?.to_string();
    let name = display_name(title);
    let tags: Vec<String> = page
        .get("categories")
        .and_then(Value::as_array)
        .map(|categories| {
            categories
                .iter()
                .filter_map(|category| category.get("title").and_then(Value::as_str))
                .map(strip_category)
                .collect()
        })
        .unwrap_or_default();
    let kind = classify(&name, &tags);
    let extmetadata = info.get("extmetadata").cloned().unwrap_or(Value::Null);
    let license = License::from_commons(
        extmeta(&extmetadata, "LicenseShortName").as_deref(),
        extmeta(&extmetadata, "LicenseUrl").as_deref(),
        extmeta(&extmetadata, "UsageTerms").as_deref(),
    );
    let author = extmeta(&extmetadata, "Artist")
        .and_then(|author| author_of(&author))
        .unwrap_or_default();
    let attribution = license.credit(&author, &source_url);
    let metadata = info.get("metadata").cloned().unwrap_or(Value::Null);
    let (channels, sample_rate) = audio_format(&metadata);
    Some(SampleItem {
        id: format!("{ID_PREFIX}{title}"),
        provider: PROVIDER_ID.to_string(),
        name,
        kind,
        duration_seconds: info.get("duration").and_then(Value::as_f64),
        channels,
        sample_rate,
        license,
        attribution,
        source_url,
        preview_url: None,
        download_url,
        // Commons publishes no per-file hash for the original; the store
        // records the sha256 of what it wrote instead.
        sha256: None,
        bytes: info.get("size").and_then(Value::as_u64),
        quality: Quality::Original,
        tags,
    })
}

/// `File:Snare_drum_unmuffled.ogg` becomes `Snare drum unmuffled`.
fn display_name(title: &str) -> String {
    let title = title
        .strip_prefix("File:")
        .or_else(|| title.strip_prefix("file:"))
        .unwrap_or(title);
    let title = title.replace('_', " ");
    match title.rfind('.') {
        Some(dot) if dot > 0 => title[..dot].to_string(),
        _ => title,
    }
}

fn strip_category(title: &str) -> String {
    title.strip_prefix("Category:").unwrap_or(title).to_string()
}

/// Whether the file is an audio container a project can decode.
fn is_sample_file(url: &str) -> bool {
    match extension(url) {
        Some(extension) => SAMPLE_EXTENSIONS.contains(&extension.as_str()),
        None => false,
    }
}

fn extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next()?;
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    if extension.is_empty() || extension.len() > 5 {
        return None;
    }
    Some(extension)
}

/// The shareable URL without the tracking query Commons appends to API links.
fn strip_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

fn normalize_title(title: &str) -> String {
    title.replace('_', " ").trim().to_string()
}

fn extmeta(extmetadata: &Value, name: &str) -> Option<String> {
    let value = extmetadata.get(name)?.get("value")?.as_str()?;
    let text = collapse_hidden_label(&strip_html(value));
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(text)
}

/// The uploader's name. Old Commons files carry "No machine-readable author
/// provided. NAME assumed (based on copyright claims)."; the name inside it is
/// the only authorship statement the file has, so it is kept.
fn author_of(author: &str) -> Option<String> {
    if let Some((_, rest)) = author.split_once("No machine-readable author provided")
        && let Some(name) = rest.split(" assumed").next()
    {
        let name = name.trim().trim_start_matches('.').trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
        return None;
    }
    if author.contains("No machine-readable author provided") {
        return None;
    }
    Some(author.trim().to_string())
}

/// Commons hides a duplicate accessible label inside the value, so stripping
/// the tags leaves the same text twice. Keep one copy.
fn collapse_hidden_label(text: &str) -> String {
    let count = text.chars().count();
    if !count.is_multiple_of(2) {
        return text.to_string();
    }
    let half = count / 2;
    let split = text.char_indices().nth(half).map(|(index, _)| index);
    match split {
        Some(index) if text[..index] == text[index..] => text[..index].to_string(),
        _ => text.to_string(),
    }
}

fn strip_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut inside = false;
    for character in value.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(character),
            _ => {}
        }
    }
    out
}

/// The channel count and sample rate a Commons file reports. The two container
/// families name the fields differently, so both spellings are searched.
fn audio_format(metadata: &Value) -> (Option<u16>, Option<u32>) {
    let mut pairs = Vec::new();
    collect_named(metadata, &mut pairs);
    let channels = first_number(&pairs, &["channels", "audio_channels"])
        .and_then(|value| u16::try_from(value).ok());
    let sample_rate = first_number(&pairs, &["sample_rate", "audio_sample_rate"])
        .and_then(|value| u32::try_from(value).ok());
    (channels, sample_rate)
}

fn collect_named<'a>(value: &'a Value, pairs: &mut Vec<(&'a str, &'a Value)>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_named(item, pairs);
            }
        }
        Value::Object(map) => {
            if let (Some(name), Some(value)) =
                (map.get("name").and_then(Value::as_str), map.get("value"))
            {
                pairs.push((name, value));
            }
            for child in map.values() {
                collect_named(child, pairs);
            }
        }
        _ => {}
    }
}

fn first_number(pairs: &[(&str, &Value)], names: &[&str]) -> Option<u64> {
    pairs
        .iter()
        .filter(|(name, _)| names.contains(name))
        .find_map(|(_, value)| value.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SampleKind;
    use crate::provider::FetchResponse;
    use std::sync::Mutex;

    struct FixtureFetch {
        body: &'static [u8],
        urls: Mutex<Vec<String>>,
    }

    impl SampleFetch for FixtureFetch {
        fn get(&self, url: &str, _max_bytes: usize) -> Result<FetchResponse, String> {
            self.urls.lock().unwrap().push(url.to_string());
            Ok(FetchResponse {
                status: 200,
                body: self.body.to_vec(),
            })
        }
    }

    fn fetch(body: &'static [u8]) -> FixtureFetch {
        FixtureFetch {
            body,
            urls: Mutex::new(Vec::new()),
        }
    }

    const SEARCH: &[u8] = include_bytes!("../../tests/fixtures/commons-search.json");
    const FILES: &[u8] = include_bytes!("../../tests/fixtures/commons-files.json");

    #[test]
    fn a_search_result_carries_its_licence_credit_and_format() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare drum".into(),
            kind: Some(SampleKind::DrumOneShot),
            limit: 10,
            ..Default::default()
        };
        let page = CommonsProvider::new().search(&query, &fetch).unwrap();

        assert_eq!(page.provider, "wikimedia-commons");
        assert_eq!(page.items.len(), 3);
        // The recorded search was the last page: it carries a category
        // continuation but no `gsroffset`, so there is no next page.
        assert_eq!(page.next_offset, None);

        let item = &page.items[0];
        assert_eq!(item.id, "commons:File:Snare drum unmuffled.ogg");
        assert_eq!(item.name, "Snare drum unmuffled");
        assert_eq!(item.kind, SampleKind::DrumOneShot);
        assert_eq!(item.duration_seconds, Some(4.816507936507937));
        assert_eq!(item.channels, Some(2));
        assert_eq!(item.sample_rate, Some(44_100));
        assert_eq!(item.bytes, Some(53_722));
        assert_eq!(item.license.id, "cc-by-sa-3.0");
        assert!(item.license.share_alike);
        assert!(item.license.check().is_ok());
        assert_eq!(item.quality, Quality::Original);
        assert!(
            item.download_url
                .starts_with("https://upload.wikimedia.org/wikipedia/commons/"),
            "{}",
            item.download_url
        );
        assert!(
            !item.download_url.contains("utm_source"),
            "the tracking query is stripped: {}",
            item.download_url
        );
        assert_eq!(
            item.source_url,
            "https://commons.wikimedia.org/wiki/File:Snare_drum_unmuffled.ogg"
        );
        assert_eq!(
            item.attribution,
            "Fixture User — CC BY-SA 3.0 (http://creativecommons.org/licenses/by-sa/3.0/) — https://commons.wikimedia.org/wiki/File:Snare_drum_unmuffled.ogg"
        );
        assert!(item.tags.contains(&"Snare drums".to_string()));
    }

    #[test]
    fn the_search_url_asks_for_the_kind_and_audio_only() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            kind: Some(SampleKind::Loop),
            tags: vec!["drums".into()],
            limit: 10,
            offset: 20,
            ..Default::default()
        };
        CommonsProvider::new().search(&query, &fetch).unwrap();
        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(url.contains("generator=search"), "{url}");
        assert!(url.contains("gsroffset=20"), "{url}");
        assert!(url.contains("gsrnamespace=6"), "{url}");
        assert!(url.contains("filetype%3Aaudio"), "{url}");
        assert!(url.contains("loop"), "{url}");
        assert!(url.contains("%22drums%22"), "{url}");
        assert!(url.contains("iiprop=url%7Csize"), "{url}");
    }

    #[test]
    fn a_duration_bound_is_applied_locally_because_the_index_has_no_duration() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            max_seconds: Some(4.5),
            limit: 10,
            ..Default::default()
        };
        let page = CommonsProvider::new().search(&query, &fetch).unwrap();
        assert_eq!(page.items.len(), 1, "only the 4.16 s snare survives");
        assert_eq!(page.items[0].name, "Snare drum muffled");
    }

    #[test]
    fn a_midi_file_is_not_a_sample() {
        let body = br#"{"batchcomplete":true,"continue":{"gsroffset":4},"query":{"pages":[
            {"title":"File:Backbeat.mid","imageinfo":[{"url":"https://upload.wikimedia.org/a/Backbeat.mid","size":900,"descriptionurl":"https://commons.wikimedia.org/wiki/File:Backbeat.mid","metadata":[]}]},
            {"title":"File:Snare.ogg","imageinfo":[{"url":"https://upload.wikimedia.org/a/Snare.ogg","size":900,"descriptionurl":"https://commons.wikimedia.org/wiki/File:Snare.ogg","metadata":[]}]}
        ]}}"#;
        let fetch = fetch(body);
        let page = CommonsProvider::new()
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        assert_eq!(page.items.len(), 1, "the MIDI sequence is dropped");
        assert_eq!(page.items[0].name, "Snare");
    }

    #[test]
    fn a_page_ends_where_the_returned_results_end() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            limit: 2,
            ..Default::default()
        };
        let page = CommonsProvider::new().search(&query, &fetch).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(
            page.next_offset,
            Some(2),
            "the third raw result starts the next page"
        );
    }

    #[test]
    fn a_channel_bound_drops_the_other_results() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            channels: Some(1),
            limit: 10,
            ..Default::default()
        };
        let page = CommonsProvider::new().search(&query, &fetch).unwrap();
        assert!(page.items.is_empty());
    }

    #[test]
    fn resolving_an_id_prefers_the_file_it_names() {
        let fetch = fetch(FILES);
        let item = CommonsProvider::new()
            .resolve("commons:File:Udu Sound 99.wav", &fetch)
            .unwrap();
        assert_eq!(item.name, "Udu Sound 99");
        assert_eq!(item.license.id, "cc-by-4.0");
        assert_eq!(item.channels, Some(2));
        assert_eq!(item.sample_rate, Some(44_100));
        assert_eq!(item.attribution.split(" — ").next(), Some("Fixture Artist"));
        assert!(fetch.urls.lock().unwrap()[0].contains("titles=File%3AUdu%20Sound%2099.wav"));
    }

    #[test]
    fn a_public_domain_file_needs_no_credit_and_the_hidden_label_is_dropped() {
        let fetch = fetch(FILES);
        let item = CommonsProvider::new()
            .resolve("commons:File:Drum - Cadence A.ogg", &fetch)
            .unwrap();
        assert_eq!(item.license.id, "public-domain");
        assert!(!item.license.attribution_required);
        assert!(
            item.attribution
                .starts_with("Unknown author — Public domain"),
            "{}",
            item.attribution
        );
        assert_eq!(item.kind, SampleKind::DrumOneShot);
    }

    #[test]
    fn an_id_from_another_provider_is_refused_with_the_expected_shape() {
        let fetch = fetch(FILES);
        let error = CommonsProvider::new()
            .resolve("freesound:14854", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("commons:File:"), "{error}");
        assert!(
            fetch.urls.lock().unwrap().is_empty(),
            "a refusal fetches nothing"
        );
    }

    #[test]
    fn a_vanished_file_is_refused_with_its_title() {
        let body = br#"{"batchcomplete":true,"query":{"pages":[{"title":"File:Gone.ogg","missing":true}]}}"#;
        let fetch = fetch(body);
        let error = CommonsProvider::new()
            .resolve("commons:File:Gone.ogg", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("File:Gone.ogg"), "{error}");
    }

    #[test]
    fn an_empty_search_is_an_empty_page_not_an_error() {
        let body = br#"{"batchcomplete":true}"#;
        let fetch = fetch(body);
        let page = CommonsProvider::new()
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn a_tracking_query_and_anchors_are_removed_from_the_file_url() {
        assert_eq!(
            strip_query("https://upload.wikimedia.org/a/b/c.ogg?utm_source=x#frag"),
            "https://upload.wikimedia.org/a/b/c.ogg"
        );
        assert_eq!(display_name("File:A_b.flac"), "A b");
        assert_eq!(display_name("File:NoExtension"), "NoExtension");
    }
}
