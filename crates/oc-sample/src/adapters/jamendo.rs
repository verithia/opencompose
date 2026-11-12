//! Jamendo: Creative Commons music behind a client id, with no OAuth step.
//!
//! The API returns one row per track with its licence deed URL, duration,
//! artist and a streaming MP3. `client_id` is a plain query parameter, so this
//! build can wire Jamendo without an OAuth flow; the key is still required and
//! comes from `JAMENDO_CLIENT_ID` or the OS keyring.
//!
//! Jamendo serves a transcoded stream, not the master upload, so a pulled
//! track is marked [`Quality::PreviewMp3`].

use serde::Deserialize;

use crate::license::License;
use crate::model::{Quality, SampleError, SampleItem, SamplePage, SampleQuery, classify};
use crate::provider::{SampleFetch, SampleProvider, encode, get_json};
use crate::providers::jamendo::{ID_PREFIX, PROVIDER_ID};

const API: &str = "https://api.jamendo.com/v3.0/tracks/";
const INCLUDE: &str = "licenses+musicinfo";
/// Jamendo mixes a narrow band; ask for the better of its two MP3 renditions.
const AUDIO_FORMAT: &str = "mp32";

/// Adapter for one external sample market.
pub struct JamendoProvider {
    key: String,
}

impl JamendoProvider {
    /// Create the provider with a client id.
    pub fn new(key: &str) -> Self {
        Self {
            key: key.trim().to_string(),
        }
    }

    fn search_url(&self, query: &SampleQuery) -> String {
        let page_size = query.page_size();
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
            "{API}?client_id={key}&format=json&limit={page_size}&offset={offset}&search={search}&audioformat={AUDIO_FORMAT}&include={INCLUDE}",
            key = encode(&self.key),
            offset = query.offset,
            search = encode(&terms.join(" ")),
        )
    }

    fn id_url(&self, id: &str) -> String {
        format!(
            "{API}?client_id={key}&format=json&id={id}&audioformat={AUDIO_FORMAT}&include={INCLUDE}",
            key = encode(&self.key),
            id = encode(id),
        )
    }
}

impl SampleProvider for JamendoProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn label(&self) -> &'static str {
        "Jamendo"
    }

    fn search(
        &self,
        query: &SampleQuery,
        fetch: &dyn SampleFetch,
    ) -> Result<SamplePage, SampleError> {
        let body = get_json(fetch, "Jamendo", &self.search_url(query))?;
        let response: TracksResponse = serde_json::from_value(body)
            .map_err(|error| SampleError::Failed(format!("Jamendo: unreadable search: {error}")))?;
        let results_count = response.headers.results_count;
        check_status(&response.headers)?;
        let items: Vec<SampleItem> = response.results.iter().map(item_from_track).collect();
        let page_size = query.page_size();
        let next_offset =
            (items.len() >= page_size).then(|| query.offset.saturating_add(items.len()));
        Ok(SamplePage {
            provider: PROVIDER_ID.to_string(),
            offset: query.offset,
            limit: page_size,
            total: Some(results_count),
            next_offset,
            items,
        })
    }

    fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError> {
        let number = id.strip_prefix(ID_PREFIX).ok_or_else(|| {
            SampleError::refused(
                "unknown_id",
                format!(
                    "'{id}' is not a Jamendo id. Search first and pass the id from the result, for example jamendo:123456."
                ),
            )
        })?;
        if number.trim().is_empty()
            || !number
                .trim()
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            return Err(SampleError::refused(
                "unknown_id",
                format!("Jamendo ids are numbers; '{number}' is not one."),
            ));
        }
        let body = get_json(fetch, "Jamendo", &self.id_url(number.trim()))?;
        let response: TracksResponse = serde_json::from_value(body)
            .map_err(|error| SampleError::Failed(format!("Jamendo: unreadable track: {error}")))?;
        check_status(&response.headers)?;
        match response.results.first() {
            Some(track) => Ok(item_from_track(track)),
            None => Err(SampleError::refused(
                "unknown_id",
                format!("Jamendo has no track {number}; it may have been removed."),
            )),
        }
    }
}

fn check_status(headers: &Headers) -> Result<(), SampleError> {
    if headers.status.eq_ignore_ascii_case("success") {
        return Ok(());
    }
    Err(SampleError::Failed(format!(
        "Jamendo refused the request: {}{}",
        headers.error_message,
        if headers.code == 0 {
            String::new()
        } else {
            format!(" (code {})", headers.code)
        }
    )))
}

#[derive(Debug, Deserialize)]
struct TracksResponse {
    #[serde(default)]
    headers: Headers,
    #[serde(default)]
    results: Vec<Track>,
}

#[derive(Debug, Default, Deserialize)]
struct Headers {
    #[serde(default)]
    status: String,
    #[serde(default)]
    code: i64,
    #[serde(default)]
    error_message: String,
    #[serde(default)]
    results_count: u64,
}

#[derive(Debug, Deserialize)]
struct Track {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    license_ccurl: String,
    #[serde(default)]
    audio: String,
    #[serde(default)]
    audiodownload: String,
    #[serde(default)]
    audiodownload_allowed: bool,
    #[serde(default)]
    musicinfo: MusicInfo,
}

#[derive(Debug, Default, Deserialize)]
struct MusicInfo {
    #[serde(default)]
    tags: MusicTags,
}

#[derive(Debug, Default, Deserialize)]
struct MusicTags {
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    instruments: Vec<String>,
}

fn item_from_track(track: &Track) -> SampleItem {
    let name = if track.name.trim().is_empty() {
        "Untitled".to_string()
    } else {
        track.name.trim().to_string()
    };
    let tags: Vec<String> = track
        .musicinfo
        .tags
        .genres
        .iter()
        .chain(track.musicinfo.tags.instruments.iter())
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    let license = if track.license_ccurl.trim().is_empty() {
        License::unknown("Unknown licence")
    } else {
        License::from_license_url(&track.license_ccurl)
    };
    let source_url = format!("https://www.jamendo.com/track/{}", track.id);
    let preview_url = non_empty(&track.audio);
    let download_url = if track.audiodownload_allowed && !track.audiodownload.trim().is_empty() {
        track.audiodownload.trim().to_string()
    } else {
        String::new()
    };
    let attribution = license.credit(&track.artist_name, &source_url);
    SampleItem {
        id: format!("{ID_PREFIX}{}", track.id),
        provider: PROVIDER_ID.to_string(),
        kind: classify(&name, &tags),
        duration_seconds: track.duration.map(|seconds| seconds as f64),
        channels: None,
        sample_rate: None,
        license,
        attribution,
        source_url,
        preview_url,
        download_url,
        sha256: None,
        bytes: None,
        quality: Quality::PreviewMp3,
        name,
        tags,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
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

    const SEARCH: &[u8] = include_bytes!("../../tests/fixtures/jamendo-search.json");

    #[test]
    fn a_track_maps_its_licence_duration_and_stream() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "night".into(),
            limit: 20,
            ..Default::default()
        };
        let page = JamendoProvider::new("CLIENT")
            .search(&query, &fetch)
            .unwrap();
        assert_eq!(page.provider, "jamendo");
        assert_eq!(page.total, Some(2));
        assert_eq!(page.items.len(), 2);

        let track = &page.items[0];
        assert_eq!(track.id, "jamendo:123456");
        assert_eq!(track.name, "Night Drive");
        assert_eq!(track.kind, SampleKind::InstrumentPhrase);
        assert_eq!(track.duration_seconds, Some(214.0));
        assert_eq!(track.license.id, "cc-by-4.0");
        assert!(track.license.check().is_ok());
        assert_eq!(track.quality, Quality::PreviewMp3);
        assert_eq!(
            track.download_url,
            "https://prod-1.storage.jamendo.com/dl/123456"
        );
        assert_eq!(
            track.preview_url.as_deref(),
            Some("https://prod-1.storage.jamendo.com/?trackid=123456&format=mp32")
        );
        assert_eq!(
            track.attribution,
            "The Frozenauts — CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/) — https://www.jamendo.com/track/123456"
        );
        assert!(track.tags.contains(&"electronic".to_string()));
        assert!(track.tags.contains(&"guitar".to_string()));
        assert!(fetch.urls.lock().unwrap()[0].contains("client_id=CLIENT"));
    }

    #[test]
    fn a_non_commercial_track_is_visible_but_unpullable() {
        let fetch = fetch(SEARCH);
        let page = JamendoProvider::new("CLIENT")
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        let nc = &page.items[1];
        assert_eq!(nc.license.id, "cc-by-nc-3.0");
        assert!(!nc.license_ok());
        // A track that does not allow downloads still streams.
        assert!(nc.download_url.is_empty());
        assert!(nc.preview_url.is_some());
    }

    #[test]
    fn the_search_url_carries_the_key_offset_and_terms() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "drum loop".into(),
            kind: Some(SampleKind::Loop),
            offset: 40,
            limit: 20,
            ..Default::default()
        };
        JamendoProvider::new("CLIENT")
            .search(&query, &fetch)
            .unwrap();
        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(
            url.starts_with("https://api.jamendo.com/v3.0/tracks/?"),
            "{url}"
        );
        assert!(url.contains("offset=40"), "{url}");
        assert!(url.contains("limit=20"), "{url}");
        assert!(url.contains("loop"), "{url}");
        assert!(url.contains("include=licenses+musicinfo"), "{url}");
    }

    #[test]
    fn an_api_error_is_reported_not_treated_as_an_empty_page() {
        let body = br#"{"headers":{"status":"failed","code":5,"error_message":"invalid client id","results_count":0},"results":[]}"#;
        let fetch = fetch(body);
        let error = JamendoProvider::new("BAD")
            .search(&SampleQuery::default(), &fetch)
            .unwrap_err();
        assert!(error.to_string().contains("invalid client id"), "{error}");
    }

    #[test]
    fn resolving_an_id_reads_one_track() {
        let body = br#"{"headers":{"status":"success","results_count":1},"results":[
            {"id":"42","name":"Bed","artist_name":"Someone","duration":30,"license_ccurl":"https://creativecommons.org/licenses/by-sa/4.0/","audio":"https://prod-1.storage.jamendo.com/?trackid=42","audiodownload_allowed":true,"audiodownload":"https://prod-1.storage.jamendo.com/dl/42"}
        ]}"#;
        let fetch = fetch(body);
        let item = JamendoProvider::new("CLIENT")
            .resolve("jamendo:42", &fetch)
            .unwrap();
        assert_eq!(item.name, "Bed");
        assert_eq!(item.license.id, "cc-by-sa-4.0");
        assert!(item.license.share_alike);
        assert_eq!(
            item.download_url,
            "https://prod-1.storage.jamendo.com/dl/42"
        );
        assert!(fetch.urls.lock().unwrap()[0].contains("id=42"));
    }

    #[test]
    fn a_non_numeric_id_is_refused_before_any_request() {
        let fetch = fetch(SEARCH);
        let error = JamendoProvider::new("CLIENT")
            .resolve("jamendo:abc", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("numbers"), "{error}");
        assert!(fetch.urls.lock().unwrap().is_empty());
    }
}
