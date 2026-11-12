//! Openverse: one search over Freesound, Jamendo, Wikimedia and others.
//!
//! Openverse answers anonymously, so it is the widest market this build can
//! reach without a key. It is an aggregator, not a library of its own: every
//! result names its `source` provider and carries that source's licence,
//! duration, file size and direct audio URL.
//!
//! The licence travels per result, so a page can mix CC0, CC BY and
//! non-commercial files. The licence rule decides what may enter a project and
//! the rest stay visible but unpullable.

use serde::Deserialize;

use crate::license::License;
use crate::model::{Quality, SampleError, SampleItem, SamplePage, SampleQuery, classify};
use crate::provider::{SampleFetch, SampleProvider, encode, get_json};
use crate::providers::openverse::{ID_PREFIX, PROVIDER_ID};

const API: &str = "https://api.openverse.org/v1/audio/";

/// Adapter for one external sample market.
pub struct OpenverseProvider;

impl OpenverseProvider {
    /// Create the provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenverseProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleProvider for OpenverseProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn label(&self) -> &'static str {
        "Openverse"
    }

    fn search(
        &self,
        query: &SampleQuery,
        fetch: &dyn SampleFetch,
    ) -> Result<SamplePage, SampleError> {
        let body = get_json(fetch, "Openverse", &search_url(query))?;
        let response: SearchResponse = serde_json::from_value(body).map_err(|error| {
            SampleError::Failed(format!("Openverse: unreadable search: {error}"))
        })?;
        let page_size = query.page_size();
        let skip = query.offset % page_size;
        let items: Vec<SampleItem> = response
            .results
            .iter()
            .skip(skip)
            .map(item_from_result)
            .collect();
        let next_offset =
            (response.page < response.page_count).then(|| query.offset.saturating_add(items.len()));
        Ok(SamplePage {
            provider: PROVIDER_ID.to_string(),
            offset: query.offset,
            limit: page_size,
            total: Some(response.result_count),
            next_offset,
            items,
        })
    }

    fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError> {
        let uuid = id.strip_prefix(ID_PREFIX).ok_or_else(|| {
            SampleError::refused(
                "unknown_id",
                format!(
                    "'{id}' is not an Openverse id. Search first and pass the id from the result, for example openverse:9f0c6b7a-1d2e-4c3b-8a90-123456789abc."
                ),
            )
        })?;
        if uuid.trim().is_empty() {
            return Err(SampleError::refused(
                "unknown_id",
                "An Openverse id needs the uuid that follow 'openverse:'.",
            ));
        }
        let body = get_json(fetch, "Openverse", &detail_url(uuid))?;
        let result: AudioResult = serde_json::from_value(body).map_err(|error| {
            SampleError::Failed(format!("Openverse: unreadable audio: {error}"))
        })?;
        let item = item_from_result(&result);
        if item.download_url.trim().is_empty() {
            return Err(SampleError::refused(
                "no_download",
                format!(
                    "Openverse has no audio file for '{}'; pick another result.",
                    item.name
                ),
            ));
        }
        Ok(item)
    }
}

fn search_url(query: &SampleQuery) -> String {
    let page_size = query.page_size();
    let page = query.offset / page_size + 1;
    let mut terms: Vec<String> = query
        .kind
        .map(|kind| {
            kind.search_terms()
                .iter()
                .map(|term| (*term).to_string())
                .collect()
        })
        .unwrap_or_default();
    let text = query.text.trim();
    if !text.is_empty() {
        terms.push(text.to_string());
    }
    format!(
        "{API}?q={query}&page={page}&page_size={page_size}",
        query = encode(&terms.join(" ")),
    )
}

/// The id is caller-supplied, so it is encoded as one path segment: a `/`,
/// `?` or `#` in it must not reroute the request.
fn detail_url(uuid: &str) -> String {
    format!("{API}{}/", encode(uuid))
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    result_count: u64,
    #[serde(default)]
    page_count: u64,
    #[serde(default)]
    page: u64,
    #[serde(default)]
    results: Vec<AudioResult>,
}

#[derive(Debug, Deserialize)]
struct AudioResult {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    creator: Option<String>,
    #[serde(default)]
    foreign_landing_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    filesize: Option<u64>,
    #[serde(default)]
    filetype: Option<String>,
    /// Milliseconds, as the API reports it.
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    channels: Option<u16>,
    #[serde(default)]
    license: String,
    #[serde(default)]
    license_version: Option<String>,
    #[serde(default)]
    license_url: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    alt_files: Vec<AltFile>,
}

#[derive(Debug, Deserialize)]
struct Tag {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct AltFile {
    #[serde(default)]
    url: Option<String>,
}

fn item_from_result(result: &AudioResult) -> SampleItem {
    let license = license_of(result);
    let source_url = result
        .foreign_landing_url
        .clone()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| format!("https://openverse.org/audio/{}", result.id));
    let mut tags: Vec<String> = result
        .tags
        .iter()
        .map(|tag| tag.name.clone())
        .filter(|name| !name.trim().is_empty())
        .collect();
    if let Some(category) = result.category.as_deref().filter(|name| !name.is_empty()) {
        tags.push(category.to_string());
    }
    let name = if result.title.trim().is_empty() {
        "Untitled".to_string()
    } else {
        result.title.trim().to_string()
    };
    let download_url = download_url(result);
    let attribution = license.credit(result.creator.as_deref().unwrap_or_default(), &source_url);
    SampleItem {
        id: format!("{ID_PREFIX}{}", result.id),
        provider: PROVIDER_ID.to_string(),
        kind: classify(&name, &tags),
        duration_seconds: result.duration.map(|millis| millis as f64 / 1000.0),
        channels: result.channels,
        sample_rate: result.sample_rate,
        license,
        attribution,
        source_url,
        preview_url: Some(download_url.clone()).filter(|url| !url.is_empty()),
        download_url,
        // Openverse reports a size but publishes no checksum; the store records
        // the sha256 of what it wrote.
        sha256: None,
        bytes: result.filesize,
        quality: quality_of(result),
        name,
        tags,
    }
}

/// The `url` field is the playable audio Openverse indexes; `alt_files` holds
/// other renditions of the same recording, used only when `url` is absent.
fn download_url(result: &AudioResult) -> String {
    result
        .url
        .clone()
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            result
                .alt_files
                .iter()
                .filter_map(|file| file.url.clone())
                .find(|url| !url.trim().is_empty())
        })
        .unwrap_or_default()
}

fn license_of(result: &AudioResult) -> License {
    let name = license_name(&result.license, result.license_version.as_deref());
    match result.license_url.as_deref().filter(|url| !url.is_empty()) {
        Some(url) => License::from_deed(&name, url),
        None => License::from_deed(&name, ""),
    }
}

fn license_name(slug: &str, version: Option<&str>) -> String {
    let version = version
        .filter(|version| !version.is_empty())
        .unwrap_or("4.0");
    match slug.to_ascii_lowercase().as_str() {
        "cc0" => "CC0 1.0".to_string(),
        "pdm" | "publicdomain" => "Public domain mark".to_string(),
        "by" => format!("CC BY {version}"),
        "by-sa" => format!("CC BY-SA {version}"),
        "by-nc" => format!("CC BY-NC {version}"),
        "by-nc-sa" => format!("CC BY-NC-SA {version}"),
        "by-nd" => format!("CC BY-ND {version}"),
        "by-nc-nd" => format!("CC BY-NC-ND {version}"),
        "sampling+" => "Sampling+ 1.0".to_string(),
        "nc-sampling+" => "NonCommercial Sampling+ 1.0".to_string(),
        other => format!("Openverse licence '{other}'"),
    }
}

/// Freesound's Openverse entries point at the 128 kbps preview; every other
/// source exposes the file it indexes.
fn quality_of(result: &AudioResult) -> Quality {
    let source = result
        .source
        .as_deref()
        .or(result.provider.as_deref())
        .unwrap_or_default();
    let mp3 = result
        .filetype
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("mp3"));
    if mp3 && source.eq_ignore_ascii_case("freesound") {
        Quality::PreviewMp3
    } else {
        Quality::Original
    }
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

    const SEARCH: &[u8] = include_bytes!("../../tests/fixtures/openverse-search.json");

    #[test]
    fn a_result_carries_its_source_licence_and_duration() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            limit: 3,
            ..Default::default()
        };
        let page = OpenverseProvider::new().search(&query, &fetch).unwrap();

        assert_eq!(page.provider, "openverse");
        assert_eq!(page.total, Some(240));
        assert_eq!(page.items.len(), 3);

        let snare = &page.items[0];
        assert_eq!(snare.id, "openverse:03fa3080-bcb3-423f-a781-47dc20bedd59");
        assert_eq!(snare.name, "Snare 4.wav");
        assert_eq!(snare.kind, SampleKind::DrumOneShot);
        assert_eq!(snare.duration_seconds, Some(0.377));
        // Openverse leaves the technical fields null when the source does not
        // publish them; the parser must keep them unknown, not invent a rate.
        assert_eq!(snare.channels, None);
        assert_eq!(snare.sample_rate, None);
        assert_eq!(snare.bytes, Some(10_384));
        assert_eq!(snare.license.id, "cc-by-3.0");
        assert!(snare.license.check().is_ok());
        assert_eq!(
            snare.download_url,
            "https://cdn.freesound.org/previews/26/26903_113878-hq.mp3"
        );
        assert_eq!(
            snare.preview_url.as_deref(),
            Some(snare.download_url.as_str())
        );
        assert_eq!(snare.quality, Quality::PreviewMp3);
        assert_eq!(
            snare.attribution,
            "Fixture Creator \u{2014} CC BY 3.0 (https://creativecommons.org/licenses/by/3.0/) \u{2014} https://freesound.org/people/FixtureCreator/sounds/26903"
        );
        assert!(snare.tags.contains(&"drum".to_string()));
        assert!(snare.tags.contains(&"snare".to_string()));
    }

    #[test]
    fn a_non_commercial_result_is_visible_but_refused_on_pull() {
        // The recorded page is all CC BY, so the refusal path keeps its own
        // body in the shape the API returns.
        let body = br#"{"result_count":1,"page_count":1,"page":1,"results":[
            {"id":"night-drive","title":"Night Drive","creator":"Frozenauts","license":"by-nc","license_version":"4.0","license_url":"https://creativecommons.org/licenses/by-nc/4.0/","url":"https://example.org/night.mp3","filetype":"mp3","source":"jamendo","duration":214000,"tags":[{"name":"synthesizer"}]}
        ]}"#;
        let fetch = fetch(body);
        let page = OpenverseProvider::new()
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        let nc = page
            .items
            .iter()
            .find(|item| item.license.id == "cc-by-nc-4.0")
            .expect("the body carries a non-commercial track");
        assert!(!nc.license_ok());
        assert_eq!(
            nc.license.check().unwrap_err().code(),
            Some("license_unsupported")
        );
        assert_eq!(nc.quality, Quality::Original);
        assert_eq!(nc.name, "Night Drive");
        assert_eq!(nc.kind, SampleKind::InstrumentPhrase);
    }

    #[test]
    fn a_wikimedia_result_keeps_its_share_alike_terms() {
        let body = br#"{"result_count":1,"page_count":1,"page":1,"results":[
            {"id":"wikimedia-1","title":"Snare drum unmuffled","license":"by-sa","license_version":"3.0","license_url":"http://creativecommons.org/licenses/by-sa/3.0/","url":"https://upload.wikimedia.org/wikipedia/commons/b/b2/Snare_drum_unmuffled.ogg","filetype":"ogg","source":"wikimedia","duration":4816,"tags":[{"name":"snare"}]}
        ]}"#;
        let fetch = fetch(body);
        let page = OpenverseProvider::new()
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        let ogg = page
            .items
            .iter()
            .find(|item| item.id.ends_with("wikimedia-1"))
            .expect("the body carries a Wikimedia file");
        assert_eq!(ogg.license.id, "cc-by-sa-3.0");
        assert!(ogg.license.share_alike);
        assert!(ogg.license.check().is_ok());
        assert!(ogg.download_url.ends_with(".ogg"));
    }

    #[test]
    fn the_search_url_pages_and_hides_nothing_by_licence() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "drum loop".into(),
            kind: Some(SampleKind::Loop),
            offset: 20,
            limit: 10,
            ..Default::default()
        };
        OpenverseProvider::new().search(&query, &fetch).unwrap();
        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(
            url.starts_with("https://api.openverse.org/v1/audio/?"),
            "{url}"
        );
        assert!(url.contains("page=3"), "{url}");
        assert!(url.contains("page_size=10"), "{url}");
        assert!(url.contains("loop"), "{url}");
        assert!(url.contains("drum"), "{url}");
        // No licence filter: a refused result must stay visible.
        assert!(!url.contains("license="), "{url}");
        assert!(!url.contains("token="), "{url}");
    }

    #[test]
    fn the_last_page_reports_no_next_offset() {
        let body = br#"{"result_count":1,"page_count":1,"page":1,"results":[
            {"id":"a","title":"Kick","license":"cc0","license_url":"https://creativecommons.org/publicdomain/zero/1.0/","url":"https://example.org/kick.wav"}
        ]}"#;
        let fetch = fetch(body);
        let page = OpenverseProvider::new()
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        assert_eq!(page.next_offset, None);
        assert_eq!(page.total, Some(1));
        assert_eq!(page.items[0].license.id, "cc0-1.0");
    }

    #[test]
    fn resolving_an_id_reads_the_audio_resource() {
        let body = br#"{"id":"abc","title":"Snare","license":"by","license_version":"4.0","license_url":"https://creativecommons.org/licenses/by/4.0/","url":"https://example.org/snare.wav","duration":1500,"creator":"Someone","foreign_landing_url":"https://example.org/snare"}"#;
        let fetch = fetch(body);
        let item = OpenverseProvider::new()
            .resolve("openverse:abc", &fetch)
            .unwrap();
        assert_eq!(item.name, "Snare");
        assert_eq!(item.duration_seconds, Some(1.5));
        assert_eq!(item.license.id, "cc-by-4.0");
        assert!(fetch.urls.lock().unwrap()[0].ends_with("/audio/abc/"));
    }

    #[test]
    fn an_id_with_path_characters_stays_one_path_segment() {
        let body = br#"{"id":"abc","title":"Snare","license":"by","license_url":"https://creativecommons.org/licenses/by/4.0/","url":"https://example.org/snare.wav"}"#;
        let fetch = fetch(body);
        OpenverseProvider::new()
            .resolve("openverse:abc/../def?x=1#frag", &fetch)
            .unwrap();
        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(
            url.ends_with("/audio/abc%2F..%2Fdef%3Fx%3D1%23frag/"),
            "{url}"
        );
        assert!(!url.contains("?x=1"), "the id cannot add a query: {url}");
        assert!(!url.contains('#'), "the id cannot add a fragment: {url}");
    }

    #[test]
    fn a_wrong_or_empty_id_is_refused_with_the_expected_shape() {
        let fetch = fetch(SEARCH);
        let error = OpenverseProvider::new()
            .resolve("commons:File:X.ogg", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("openverse:"), "{error}");
        let error = OpenverseProvider::new()
            .resolve("openverse:", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(fetch.urls.lock().unwrap().is_empty());
    }
}
