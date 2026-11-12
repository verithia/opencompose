//! Internet Archive: a search index over millions of audio items, no key.
//!
//! The advanced search returns item metadata but not the file list, so a
//! search result names the item and its licence while the pull reads
//! `metadata/<id>` and picks one audio file. That keeps one search to one
//! request instead of one request per result.
//!
//! The licence is per item (`metadata.licenseurl`). An item without one is
//! visible and refused by the licence rule, never guessed.

use serde::Deserialize;
use serde_json::Value;

use crate::license::License;
use crate::model::{Quality, SampleError, SampleItem, SamplePage, SampleQuery, classify};
use crate::provider::{SampleFetch, SampleProvider, encode, get_json};
use crate::providers::internet_archive::{ID_PREFIX, PROVIDER_ID};

const SEARCH_API: &str = "https://archive.org/advancedsearch.php";
const METADATA_API: &str = "https://archive.org/metadata";
/// Containers the engine can decode. Other files in an item (text, images,
/// video) are ignored when the pull picks its audio.
const SAMPLE_EXTENSIONS: [&str; 7] = ["wav", "flac", "mp3", "ogg", "oga", "aif", "aiff"];

/// Adapter for one external sample market.
pub struct InternetArchiveProvider;

impl InternetArchiveProvider {
    /// Create the provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for InternetArchiveProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleProvider for InternetArchiveProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn label(&self) -> &'static str {
        "Internet Archive"
    }

    fn search(
        &self,
        query: &SampleQuery,
        fetch: &dyn SampleFetch,
    ) -> Result<SamplePage, SampleError> {
        let body = get_json(fetch, "Internet Archive", &search_url(query))?;
        let response: SearchResponse = serde_json::from_value(body).map_err(|error| {
            SampleError::Failed(format!("Internet Archive: unreadable search: {error}"))
        })?;
        let page_size = query.page_size();
        let docs = response.response.docs;
        let items: Vec<SampleItem> = docs.iter().take(page_size).map(item_from_doc).collect();
        let seen = response.response.start + items.len() as u64;
        let next_offset =
            (response.response.num_found > seen).then(|| query.offset.saturating_add(items.len()));
        Ok(SamplePage {
            provider: PROVIDER_ID.to_string(),
            offset: query.offset,
            limit: page_size,
            total: Some(response.response.num_found),
            next_offset,
            items,
        })
    }

    fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError> {
        let identifier = id.strip_prefix(ID_PREFIX).ok_or_else(|| {
            SampleError::refused(
                "unknown_id",
                format!(
                    "'{id}' is not an Internet Archive id. Search first and pass the id from the result, for example ia:prelinger-snare."
                ),
            )
        })?;
        if identifier.trim().is_empty() {
            return Err(SampleError::refused(
                "unknown_id",
                "An Internet Archive id needs the item identifier after 'ia:'.",
            ));
        }
        let body = get_json(
            fetch,
            "Internet Archive",
            &format!("{METADATA_API}/{}", encode(identifier)),
        )?;
        let response: MetadataResponse = serde_json::from_value(body).map_err(|error| {
            SampleError::Failed(format!("Internet Archive: unreadable metadata: {error}"))
        })?;
        let file = pick_audio(&response.files).ok_or_else(|| {
            SampleError::refused(
                "unknown_id",
                format!(
                    "Internet Archive item '{identifier}' has no playable audio file; it may be text, video or images."
                ),
            )
        })?;
        let identifier = response
            .metadata
            .identifier
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(identifier);
        let name = response
            .metadata
            .title
            .as_ref()
            .and_then(text_of)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| identifier.to_string());
        let license = match response.metadata.licenseurl.as_ref().and_then(text_of) {
            Some(url) => License::from_license_url(&url),
            None => License::unknown("Unknown licence"),
        };
        let author = response.metadata.creator.as_ref().and_then(text_of);
        let source_url = details_url(identifier);
        let tags = subjects(response.metadata.subject.as_ref());
        let download_url = download_url(identifier, &file.name);
        let attribution = license.credit(author.as_deref().unwrap_or_default(), &source_url);
        Ok(SampleItem {
            id: format!("{ID_PREFIX}{identifier}"),
            provider: PROVIDER_ID.to_string(),
            kind: classify(&name, &tags),
            duration_seconds: file.length.as_deref().and_then(seconds),
            channels: None,
            sample_rate: None,
            license,
            attribution,
            source_url,
            preview_url: Some(download_url.clone()),
            download_url,
            sha256: None,
            bytes: file
                .size
                .as_deref()
                .and_then(|size| size.trim().parse().ok()),
            quality: Quality::Original,
            name,
            tags,
        })
    }
}

fn search_url(query: &SampleQuery) -> String {
    let page_size = query.page_size();
    let page = query.offset / page_size + 1;
    let mut clauses: Vec<String> = vec!["mediatype:audio".to_string()];
    if let Some(kind) = query.kind
        && !kind.search_terms().is_empty()
    {
        clauses.push(format!("({})", kind.search_terms().join(" OR ")));
    }
    let text = query.text.trim();
    if !text.is_empty() {
        clauses.push(format!("({text})"));
    }
    format!(
        "{SEARCH_API}?q={q}&fl%5B%5D=identifier&fl%5B%5D=title&fl%5B%5D=creator&fl%5B%5D=licenseurl&fl%5B%5D=subject&rows={page_size}&page={page}&output=json",
        q = encode(&clauses.join(" AND ")),
    )
}

fn details_url(identifier: &str) -> String {
    format!("https://archive.org/details/{identifier}")
}

fn download_url(identifier: &str, name: &str) -> String {
    format!(
        "https://archive.org/download/{identifier}/{}",
        encode_path(name)
    )
}

/// Encode each path segment, keeping the separators an item file name uses.
fn encode_path(name: &str) -> String {
    name.split('/')
        .map(encode)
        .collect::<Vec<String>>()
        .join("/")
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    response: SearchBody,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchBody {
    #[serde(default)]
    num_found: u64,
    #[serde(default)]
    start: u64,
    #[serde(default)]
    docs: Vec<Doc>,
}

#[derive(Debug, Deserialize)]
struct Doc {
    identifier: String,
    #[serde(default)]
    title: Option<Value>,
    #[serde(default)]
    creator: Option<Value>,
    #[serde(default)]
    licenseurl: Option<Value>,
    #[serde(default)]
    subject: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct MetadataResponse {
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    files: Vec<FileEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct Metadata {
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    title: Option<Value>,
    #[serde(default)]
    creator: Option<Value>,
    #[serde(default)]
    licenseurl: Option<Value>,
    #[serde(default)]
    subject: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct FileEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    length: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    private: Option<String>,
}

fn item_from_doc(doc: &Doc) -> SampleItem {
    let identifier = doc.identifier.trim();
    let name = doc
        .title
        .as_ref()
        .and_then(text_of)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| identifier.to_string());
    let license = match doc.licenseurl.as_ref().and_then(text_of) {
        Some(url) => License::from_license_url(&url),
        None => License::unknown("Unknown licence"),
    };
    let author = doc.creator.as_ref().and_then(text_of);
    let source_url = details_url(identifier);
    let tags = subjects(doc.subject.as_ref());
    let attribution = license.credit(author.as_deref().unwrap_or_default(), &source_url);
    SampleItem {
        id: format!("{ID_PREFIX}{identifier}"),
        provider: PROVIDER_ID.to_string(),
        kind: classify(&name, &tags),
        // The advanced search index carries no duration or file size; the pull
        // reads both from the item's file list.
        duration_seconds: None,
        channels: None,
        sample_rate: None,
        license,
        attribution,
        source_url,
        preview_url: None,
        download_url: String::new(),
        sha256: None,
        bytes: None,
        quality: Quality::Original,
        name,
        tags,
    }
}

/// Prefer an original lossless file, then any audio rendition. A derivative
/// MP3 is still playable, so it is the fallback rather than a refusal.
fn pick_audio(files: &[FileEntry]) -> Option<&FileEntry> {
    files
        .iter()
        .filter_map(|file| rank(file).map(|rank| (rank, file)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, file)| file)
}

fn rank(file: &FileEntry) -> Option<u8> {
    if file.private.as_deref() == Some("true") {
        return None;
    }
    let extension = extension(&file.name)?;
    let base = match extension.as_str() {
        "wav" => 0,
        "flac" => 1,
        "aif" | "aiff" => 2,
        "ogg" | "oga" => 3,
        "mp3" => 4,
        _ => return None,
    };
    let original = file.source.as_deref() == Some("original");
    Some(if original { base } else { base + 10 })
}

fn extension(name: &str) -> Option<String> {
    let name = name.rsplit('/').next()?;
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    (!extension.is_empty() && SAMPLE_EXTENSIONS.contains(&extension.as_str())).then_some(extension)
}

/// A field the archive reports as a string or an array of strings.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Array(items) => items.iter().find_map(text_of),
        _ => None,
    }
}

fn subjects(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().filter_map(text_of).collect(),
        Some(Value::String(text)) => text
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// A file length is either seconds or `H:MM:SS`.
fn seconds(text: &str) -> Option<f64> {
    let text = text.trim();
    if let Ok(value) = text.parse::<f64>() {
        return (value.is_finite() && value >= 0.0).then_some(value);
    }
    let mut total = 0.0;
    for part in text.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(total)
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

    const SEARCH: &[u8] = include_bytes!("../../tests/fixtures/internet-archive-search.json");
    const METADATA: &[u8] = include_bytes!("../../tests/fixtures/internet-archive-metadata.json");

    #[test]
    fn a_search_maps_the_item_licence_without_fetching_its_files() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "snare".into(),
            limit: 3,
            ..Default::default()
        };
        let page = InternetArchiveProvider::new()
            .search(&query, &fetch)
            .unwrap();

        assert_eq!(page.provider, "internet-archive");
        assert_eq!(page.total, Some(14));
        assert_eq!(page.items.len(), 3);

        let first = &page.items[0];
        assert_eq!(first.id, "ia:bsdtalk138");
        assert_eq!(first.name, "bsdtalk138 - Central Syslog");
        assert_eq!(first.kind, SampleKind::Unknown);
        assert_eq!(first.license.id, "cc-by-3.0");
        assert!(first.license.check().is_ok());
        assert_eq!(
            first.attribution,
            "Fixture Creator \u{2014} CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) \u{2014} https://archive.org/details/bsdtalk138"
        );
        // A search names the item; the pull reads the file list.
        assert!(first.download_url.is_empty());
        assert_eq!(first.duration_seconds, None);
        assert!(first.tags.contains(&"BSDTalk".to_string()));
        assert_eq!(fetch.urls.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_item_without_a_licence_url_is_refused_by_the_rule() {
        let body = br#"{"response":{"numFound":1,"start":0,"docs":[
            {"identifier":"old-radio-bed","title":"Old radio bed","creator":"Someone","subject":"radio;suspense"}
        ]}}"#;
        let fetch = fetch(body);
        let page = InternetArchiveProvider::new()
            .search(&SampleQuery::default(), &fetch)
            .unwrap();
        let unknown = &page.items[0];
        assert_eq!(unknown.id, "ia:old-radio-bed");
        assert_eq!(unknown.license.id, "unknown");
        assert!(!unknown.license_ok());
        assert_eq!(
            unknown.license.check().unwrap_err().code(),
            Some("license_unsupported")
        );
        // A semicolon-separated subject string still yields tags.
        assert!(unknown.tags.contains(&"suspense".to_string()));
    }

    #[test]
    fn the_search_url_scopes_to_audio_and_pages() {
        let fetch = fetch(SEARCH);
        let query = SampleQuery {
            text: "drum loop".into(),
            kind: Some(SampleKind::Loop),
            offset: 40,
            limit: 20,
            ..Default::default()
        };
        InternetArchiveProvider::new()
            .search(&query, &fetch)
            .unwrap();
        let url = fetch.urls.lock().unwrap()[0].clone();
        assert!(
            url.starts_with("https://archive.org/advancedsearch.php?"),
            "{url}"
        );
        assert!(url.contains("mediatype%3Aaudio"), "{url}");
        assert!(url.contains("page=3"), "{url}");
        assert!(url.contains("rows=20"), "{url}");
        assert!(url.contains("loop"), "{url}");
        assert!(url.contains("fl%5B%5D=licenseurl"), "{url}");
    }

    #[test]
    fn resolving_picks_the_original_audio_and_its_declared_length() {
        let fetch = fetch(METADATA);
        let item = InternetArchiveProvider::new()
            .resolve("ia:bsdtalk138", &fetch)
            .unwrap();
        assert_eq!(item.id, "ia:bsdtalk138");
        assert_eq!(item.name, "bsdtalk138 - Central Syslog");
        assert_eq!(item.license.id, "cc-by-3.0");
        assert_eq!(item.duration_seconds, Some(449.4));
        assert_eq!(item.bytes, Some(3_595_195));
        // The original MP3 outranks the derivative Ogg Vorbis.
        assert_eq!(
            item.download_url,
            "https://archive.org/download/bsdtalk138/bsdtalk138.mp3"
        );
        assert_eq!(
            item.preview_url.as_deref(),
            Some(item.download_url.as_str())
        );
        assert_eq!(item.kind, SampleKind::Unknown);
        assert!(item.tags.contains(&"BSD".to_string()));
    }

    #[test]
    fn an_item_without_audio_is_refused_with_the_identifier() {
        let body = br#"{"metadata":{"identifier":"text-item","title":"A book"},"files":[
            {"name":"book.txt","source":"original","size":"10"},
            {"name":"cover.jpg","source":"original","size":"900"}
        ]}"#;
        let fetch = fetch(body);
        let error = InternetArchiveProvider::new()
            .resolve("ia:text-item", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("text-item"), "{error}");
        assert!(error.to_string().contains("no playable audio"), "{error}");
    }

    #[test]
    fn a_wrong_id_is_refused_before_any_request() {
        let fetch = fetch(METADATA);
        let error = InternetArchiveProvider::new()
            .resolve("openverse:abc", &fetch)
            .unwrap_err();
        assert_eq!(error.code(), Some("unknown_id"));
        assert!(error.to_string().contains("ia:"), "{error}");
        assert!(fetch.urls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_file_length_is_seconds_or_a_clock_value() {
        assert_eq!(seconds("12.5"), Some(12.5));
        assert_eq!(seconds("1:02"), Some(62.0));
        assert_eq!(seconds("1:02:03"), Some(3723.0));
        assert_eq!(seconds(""), None);
        assert_eq!(seconds("unknown"), None);
    }

    #[test]
    fn a_path_is_encoded_segment_by_segment() {
        assert_eq!(
            encode_path("disc 1/track 01.wav"),
            "disc%201/track%2001.wav"
        );
    }
}
