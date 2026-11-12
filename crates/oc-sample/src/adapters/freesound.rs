//! Freesound: the catalogue that is actually built for samples, behind an API
//! key.
//!
//! Freesound reports duration, channels, sample rate, tags and a licence per
//! sound, so a search maps onto [`SampleQuery`] almost one to one. What it
//! does not give an API key is the original file: `/sounds/<id>/download/`
//! requires an OAuth2 token, while the preview MP3s are public. This provider
//! therefore searches everything and resolves the 128 kbps preview, marked
//! [`Quality::PreviewMp3`] so the receipt and the tool result say so. A client
//! that holds an OAuth2 token rewrites the pull to the download endpoint and
//! marks the receipt [`Quality::Original`].

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::license::License;
use crate::model::{Quality, SampleError, SampleItem, SamplePage, SampleQuery, classify};
use crate::provider::{SampleFetch, SampleProvider, encode, get_json_with_headers};
use crate::providers::freesound::{ID_PREFIX, PROVIDER_ID};

const API: &str = "https://freesound.org/apiv2";
/// Every field a result uses. Naming `fields` replaces the defaults, so the
/// list has to be complete.
const FIELDS: &str =
    "id,name,tags,license,duration,samplerate,channels,filesize,previews,type,username";

/// Adapter for one external sample market.
pub struct FreesoundProvider {
    key: String,
}

impl FreesoundProvider {
    /// Create the provider with an API key.
    pub fn new(key: &str) -> Self {
        Self {
            key: key.trim().to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{API}/{path}?fields={FIELDS}")
    }

    /// Freesound's documented API authentication. The key travels in the
    /// header, never in the URL, so a query string repeated in an error or a
    /// log line cannot leak it.
    fn authorization(&self) -> String {
        format!("Token {}", self.key)
    }
}

impl SampleProvider for FreesoundProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn label(&self) -> &'static str {
        "Freesound"
    }

    fn search(
        &self,
        query: &SampleQuery,
        fetch: &dyn SampleFetch,
    ) -> Result<SamplePage, SampleError> {
        let authorization = self.authorization();
        let headers = [("Authorization", authorization.as_str())];
        let body = get_json_with_headers(fetch, "Freesound", &self.search_url(query), &headers)?;
        let response: SearchResponse = serde_json::from_value(body).map_err(|error| {
            SampleError::Failed(format!("Freesound: unreadable search response: {error}"))
        })?;
        let page_size = query.page_size();
        let skip = query.offset % page_size;
        let items: Vec<SampleItem> = response
            .results
            .iter()
            .skip(skip)
            .map(item_from_sound)
            .collect();
        let next_offset = response
            .next
            .is_some()
            .then(|| query.offset.saturating_add(items.len()));
        Ok(SamplePage {
            provider: PROVIDER_ID.to_string(),
            offset: query.offset,
            limit: page_size,
            total: Some(response.count),
            next_offset,
            items,
        })
    }

    fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError> {
        let number = id.strip_prefix(ID_PREFIX).ok_or_else(|| {
            SampleError::refused(
                "unknown_id",
                format!(
                    "'{id}' is not a Freesound id. Search first and pass the id from the result, for example freesound:14854."
                ),
            )
        })?;
        let number: u64 = number.parse().map_err(|_| {
            SampleError::refused(
                "unknown_id",
                format!("Freesound ids are numbers; '{number}' is not one."),
            )
        })?;
        let authorization = self.authorization();
        let headers = [("Authorization", authorization.as_str())];
        let body = get_json_with_headers(
            fetch,
            "Freesound",
            &self.url(&format!("sounds/{number}/")),
            &headers,
        )?;
        let sound: Sound = serde_json::from_value(body).map_err(|error| {
            SampleError::Failed(format!("Freesound: unreadable sound: {error}"))
        })?;
        Ok(item_from_sound(&sound))
    }
}

impl FreesoundProvider {
    fn search_url(&self, query: &SampleQuery) -> String {
        let page_size = query.page_size();
        let page = query.offset / page_size + 1;
        let mut terms: Vec<String> = Vec::new();
        if let Some(kind) = query.kind {
            for term in kind.search_terms() {
                terms.push((*term).to_string());
            }
        }
        let text = query.text.trim();
        if !text.is_empty() {
            terms.push(text.to_string());
        }
        let mut filters: Vec<String> = Vec::new();
        match (query.min_seconds, query.max_seconds) {
            (Some(min), Some(max)) => filters.push(format!("duration:[{min} TO {max}]")),
            (Some(min), None) => filters.push(format!("duration:[{min} TO *]")),
            (None, Some(max)) => filters.push(format!("duration:[0 TO {max}]")),
            (None, None) => {}
        }
        if let Some(channels) = query.channels {
            filters.push(format!("channels:{channels}"));
        }
        for tag in &query.tags {
            let tag = tag.trim();
            if !tag.is_empty() {
                filters.push(format!("tag:{}", encode(tag)));
            }
        }
        let mut url = format!(
            "{API}/search/text/?query={query}&fields={FIELDS}&page={page}&page_size={page_size}",
            query = encode(&terms.join(" ")),
        );
        if !filters.is_empty() {
            url.push_str(&format!("&filter={}", encode(&filters.join(" "))));
        }
        url
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    count: u64,
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    results: Vec<Sound>,
}

#[derive(Debug, Deserialize)]
struct Sound {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    license: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    samplerate: Option<u32>,
    #[serde(default)]
    channels: Option<u16>,
    #[serde(default)]
    previews: BTreeMap<String, String>,
    #[serde(default)]
    username: Option<String>,
}

fn item_from_sound(sound: &Sound) -> SampleItem {
    let license = License::from_freesound(&sound.license);
    let source_url = format!("https://freesound.org/s/{}/", sound.id);
    let author = sound.username_or_empty();
    let attribution = license.credit(author, &source_url);
    let preview_url = sound
        .previews
        .get("preview-hq-mp3")
        .or_else(|| sound.previews.get("preview-lq-mp3"))
        .cloned();
    SampleItem {
        id: format!("{ID_PREFIX}{}", sound.id),
        provider: PROVIDER_ID.to_string(),
        name: sound.name.clone(),
        kind: classify(&sound.name, &sound.tags),
        duration_seconds: sound.duration,
        channels: sound.channels,
        sample_rate: sound.samplerate,
        license,
        attribution,
        source_url,
        preview_url: preview_url.clone(),
        download_url: preview_url.unwrap_or_default(),
        // Freesound's md5 covers the original file, not the preview this build
        // pulls, so it cannot be verified here.
        sha256: None,
        bytes: None,
        quality: Quality::PreviewMp3,
        tags: sound.tags.clone(),
    }
}

impl Sound {
    fn username_or_empty(&self) -> &str {
        self.username.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::FetchResponse;
    use std::sync::Mutex;

    struct FixtureFetch {
        body: &'static [u8],
        urls: Mutex<Vec<String>>,
        headers: Mutex<Vec<(String, String)>>,
    }

    impl SampleFetch for FixtureFetch {
        fn get(&self, url: &str, max_bytes: usize) -> Result<FetchResponse, String> {
            self.get_with(url, max_bytes, &[])
        }

        fn get_with(
            &self,
            url: &str,
            _max_bytes: usize,
            headers: &[(&str, &str)],
        ) -> Result<FetchResponse, String> {
            self.urls.lock().unwrap().push(url.to_string());
            *self.headers.lock().unwrap() = headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
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
            headers: Mutex::new(Vec::new()),
        }
    }

    const SEARCH: &[u8] = include_bytes!("../../tests/fixtures/freesound-search.json");
    const SOUND: &[u8] = include_bytes!("../../tests/fixtures/freesound-sound.json");

    #[test]
    fn a_search_maps_the_sound_metadata_onto_the_item() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            limit: 15,
            ..Default::default()
        };
        let provider = FreesoundProvider::new("KEY");
        let page = provider.search(&query, &fetch).unwrap();

        assert_eq!(page.provider, "freesound");
        assert_eq!(page.total, Some(42));
        assert_eq!(page.items.len(), 3);

        let item = &page.items[0];
        assert_eq!(item.id, "freesound:14854");
        assert_eq!(item.name, "snare_01.wav");
        assert_eq!(item.kind, crate::model::SampleKind::DrumOneShot);
        assert_eq!(item.duration_seconds, Some(0.42));
        assert_eq!(item.channels, Some(1));
        assert_eq!(item.sample_rate, Some(44_100));
        assert_eq!(item.license.id, "cc0-1.0");
        assert!(!item.license.attribution_required);
        assert!(item.license.check().is_ok());
        assert_eq!(
            item.download_url,
            "https://cdn.freesound.org/previews/14/14854_100-hq.mp3"
        );
        assert_eq!(
            item.preview_url.as_deref(),
            Some(item.download_url.as_str())
        );
        assert_eq!(item.quality, Quality::PreviewMp3);
        assert_eq!(
            item.attribution,
            "bassmosphere — Creative Commons 0 — https://freesound.org/s/14854/"
        );
        assert_eq!(item.source_url, "https://freesound.org/s/14854/");
        assert!(item.tags.contains(&"snare".to_string()));
    }

    #[test]
    fn a_non_commercial_search_result_is_visible_but_not_pullable() {
        let fetch = fetch(SEARCH);
        let page = FreesoundProvider::new("KEY")
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        let nc = page
            .items
            .iter()
            .find(|item| item.id == "freesound:22222")
            .expect("the fixture carries a non-commercial sound");
        assert_eq!(nc.license.id, "cc-by-nc-4.0");
        assert!(!nc.license_ok());
        assert_eq!(
            nc.license.check().unwrap_err().code(),
            Some("license_unsupported")
        );
        // The other two are usable.
        assert!(page.items.iter().filter(|item| item.license_ok()).count() == 2);
    }

    #[test]
    fn the_search_url_carries_the_page_filters_and_no_key() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "drum loop".into(),
            kind: Some(crate::model::SampleKind::Loop),
            min_seconds: Some(0.5),
            max_seconds: Some(4.0),
            channels: Some(2),
            tags: vec!["acoustic".into()],
            offset: 30,
            limit: 15,
        };
        FreesoundProvider::new("KEY")
            .search(&query, &fetch)
            .unwrap();
        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(
            url.contains("page=3"),
            "offset 30 at 15 per page is page 3: {url}"
        );
        assert!(url.contains("page_size=15"), "{url}");
        assert!(
            url.contains("filter=duration%3A%5B0.5%20TO%204%5D"),
            "{url}"
        );
        assert!(url.contains("channels%3A2"), "{url}");
        assert!(url.contains("tag%3Aacoustic"), "{url}");
        assert!(url.contains("loop"), "{url}");
        assert!(url.contains("fields="), "{url}");
        assert!(
            !url.contains("token"),
            "the key stays out of the URL: {url}"
        );
        assert!(!url.contains("KEY"), "{url}");
    }

    #[test]
    fn the_key_travels_in_the_authorization_header() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            ..Default::default()
        };
        FreesoundProvider::new("sk-secret")
            .search(&query, &fetch)
            .unwrap();

        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(!url.contains("sk-secret"), "{url}");
        assert!(!url.contains("token"), "{url}");
        assert_eq!(
            fetch.headers.lock().unwrap().as_slice(),
            &[("Authorization".to_string(), "Token sk-secret".to_string())]
        );
    }

    #[test]
    fn resolving_sends_the_key_in_the_authorization_header() {
        let fetch = fetch(SOUND);
        FreesoundProvider::new("sk-secret")
            .resolve("freesound:14854", &fetch)
            .unwrap();

        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(!url.contains("sk-secret"), "{url}");
        assert!(!url.contains("token"), "{url}");
        assert!(url.contains("sounds/14854/"), "{url}");
        assert_eq!(
            fetch.headers.lock().unwrap().as_slice(),
            &[("Authorization".to_string(), "Token sk-secret".to_string())]
        );
    }

    #[test]
    fn an_offset_inside_a_page_skips_the_results_before_it() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            limit: 2,
            offset: 1,
            ..Default::default()
        };
        let page = FreesoundProvider::new("KEY")
            .search(&query, &fetch)
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].id, "freesound:22222");
        assert_eq!(page.next_offset, Some(3));
    }

    #[test]
    fn the_last_page_reports_no_next_offset() {
        let body = br#"{"count":1,"next":null,"results":[{"id":7,"name":"kick","license":"Creative Commons 0"}]}"#;
        let fetch = fetch(body);
        let page = FreesoundProvider::new("KEY")
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        assert_eq!(page.next_offset, None);
        assert_eq!(page.total, Some(1));
    }

    #[test]
    fn resolving_an_id_reads_the_sound_resource() {
        let fetch = fetch(SOUND);
        let item = FreesoundProvider::new("KEY")
            .resolve("freesound:14854", &fetch)
            .unwrap();
        assert_eq!(item.name, "snare_01.wav");
        assert_eq!(item.license.id, "cc0-1.0");
        assert!(fetch.urls.lock().unwrap()[0].contains("sounds/14854/"));
    }

    #[test]
    fn an_id_from_another_provider_is_refused_with_the_expected_shape() {
        let fetch = fetch(SOUND);
        let error = FreesoundProvider::new("KEY")
            .resolve("commons:File:X.ogg", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("freesound:14854"), "{error}");
        assert!(fetch.urls.lock().unwrap().is_empty());

        let error = FreesoundProvider::new("KEY")
            .resolve("freesound:abc", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("numbers"), "{error}");
    }

    #[test]
    fn a_sound_without_a_preview_has_no_download_url() {
        let body =
            br#"{"count":1,"next":null,"results":[{"id":9,"name":"misc.wav","license":"Creative Commons 0","previews":{}}]}"#;
        let fetch = fetch(body);
        let page = FreesoundProvider::new("KEY")
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        assert_eq!(page.items[0].download_url, "");
        assert_eq!(page.items[0].kind, crate::model::SampleKind::Unknown);
    }
}
