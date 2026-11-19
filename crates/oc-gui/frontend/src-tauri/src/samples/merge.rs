//! How the pages of several markets become one result list.
//!
//! Markets are interleaved round-robin in the order they were asked, so the
//! first screen shows every market's best matches instead of one market's
//! whole page. A sound reachable through two markets (an aggregator and the
//! site it indexes) appears once: two items whose landing pages normalize to
//! the same address are the same upload, and the first market asked keeps it.

use oc_sample::model::SampleItem;

/// Query parameters that only track a visit and never select a page.
const TRACKING_PARAMS: [&str; 6] = ["fbclid", "gclid", "mc_cid", "mc_eid", "ref", "ref_src"];

/// The address two listings of one sound share: no scheme, a lowercase host,
/// no fragment, no tracking parameters and no trailing slash. `None` when the
/// item names no page, which never matches another item.
pub(crate) fn landing_key(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split('#')
        .next()
        .unwrap_or_default();
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = address.split_once('/').unwrap_or((address, ""));
    let host = host.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let path = path.trim_end_matches('/');
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default();
            !name.is_empty()
                && !name.to_ascii_lowercase().starts_with("utm_")
                && !TRACKING_PARAMS
                    .iter()
                    .any(|tracking| name.eq_ignore_ascii_case(tracking))
        })
        .collect();
    let mut key = host;
    if !path.is_empty() {
        key.push('/');
        key.push_str(path);
    }
    if !kept.is_empty() {
        key.push('?');
        key.push_str(&kept.join("&"));
    }
    Some(key)
}

/// The identity used to drop a repeated sound: its landing page, or the file
/// itself when the source names no page.
fn sound_key(item: &SampleItem) -> Option<String> {
    landing_key(&item.source_url).or_else(|| landing_key(&item.download_url))
}

/// Interleave `pages` round-robin and drop every later copy of a sound. Each
/// page keeps its own order; the index of the page an item came from travels
/// with it.
pub(crate) fn interleave(pages: Vec<Vec<SampleItem>>) -> Vec<(usize, SampleItem)> {
    let mut queues: Vec<std::vec::IntoIter<SampleItem>> =
        pages.into_iter().map(Vec::into_iter).collect();
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    loop {
        let mut any = false;
        for (index, queue) in queues.iter_mut().enumerate() {
            let Some(item) = queue.next() else {
                continue;
            };
            any = true;
            if let Some(key) = sound_key(&item)
                && !seen.insert(key)
            {
                continue;
            }
            merged.push((index, item));
        }
        if !any {
            return merged;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_sample::model::{Quality, SampleKind};

    fn item(provider: &str, id: &str, source: &str) -> SampleItem {
        SampleItem {
            id: format!("{provider}:{id}"),
            provider: provider.into(),
            name: id.into(),
            kind: SampleKind::Unknown,
            duration_seconds: None,
            channels: None,
            sample_rate: None,
            license: oc_sample::license::License::cc0(),
            attribution: String::new(),
            source_url: source.into(),
            preview_url: None,
            download_url: String::new(),
            sha256: None,
            bytes: None,
            quality: Quality::Original,
            tags: Vec::new(),
        }
    }

    #[test]
    fn a_landing_page_normalizes_scheme_host_slash_and_tracking() {
        let key = landing_key("https://FreeSound.org/people/x/sounds/1/?utm_source=a&fbclid=b#top");
        assert_eq!(key.as_deref(), Some("freesound.org/people/x/sounds/1"));
        assert_eq!(landing_key("http://freesound.org/people/x/sounds/1"), key);
        // A parameter that selects the page is kept.
        assert_eq!(
            landing_key("https://example.org/track?id=7&utm_medium=x").as_deref(),
            Some("example.org/track?id=7")
        );
        // The path keeps its case: only the host is case-insensitive.
        assert_ne!(
            landing_key("https://example.org/File:A.ogg"),
            landing_key("https://example.org/file:a.ogg")
        );
        assert_eq!(landing_key(""), None);
        assert_eq!(landing_key("https:///path"), None);
    }

    #[test]
    fn markets_interleave_in_the_order_they_were_asked() {
        let merged = interleave(vec![
            vec![
                item("a", "1", "https://a.org/1"),
                item("a", "2", "https://a.org/2"),
                item("a", "3", "https://a.org/3"),
            ],
            vec![item("b", "1", "https://b.org/1")],
            vec![
                item("c", "1", "https://c.org/1"),
                item("c", "2", "https://c.org/2"),
            ],
        ]);
        let ids: Vec<&str> = merged.iter().map(|(_, item)| item.id.as_str()).collect();
        assert_eq!(ids, vec!["a:1", "b:1", "c:1", "a:2", "c:2", "a:3"]);
        assert_eq!(merged[1].0, 1);
    }

    #[test]
    fn the_same_sound_from_two_markets_is_kept_once_from_the_first_market() {
        let merged = interleave(vec![
            vec![item(
                "freesound",
                "1",
                "https://freesound.org/people/x/sounds/1/",
            )],
            vec![
                item(
                    "openverse",
                    "9",
                    "http://FREESOUND.org/people/x/sounds/1?ref=ov",
                ),
                item(
                    "openverse",
                    "10",
                    "https://freesound.org/people/x/sounds/2/",
                ),
            ],
            vec![item("c", "empty-a", ""), item("c", "empty-b", "")],
        ]);
        let ids: Vec<&str> = merged.iter().map(|(_, item)| item.id.as_str()).collect();
        // Items with no landing page or file are never merged with each other.
        assert_eq!(
            ids,
            vec!["freesound:1", "c:empty-a", "openverse:10", "c:empty-b"]
        );
    }
}
