//! What a sample's licence means for a project.
//!
//! The licence travels with the file and is shown at download time. A licence
//! that the DAW cannot represent (non-commercial, no-derivatives, GFDL, an
//! unknown deed) needs the user's explicit acceptance before the file is pulled;
//! it is not refused outright, because using a free-of-charge sample in a track
//! is the user's call and the credit line stays with the audio either way.
//!
//! The credit line returned by [`License::credit`] is written into the sidecar
//! beside the audio and repeated by every tool result, so attribution travels
//! with the file instead of living in a catalogue that a project does not ship.

use serde::{Deserialize, Serialize};

use crate::model::SampleError;

/// A licence in the form a project can carry: a machine id, the name the
/// source used, and the terms that change what the project may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    /// Machine id, for example `cc0-1.0` or `cc-by-sa-3.0`.
    pub id: String,
    /// The name exactly as the source reported it.
    pub name: String,
    pub url: Option<String>,
    pub attribution_required: bool,
    pub share_alike: bool,
    pub non_commercial: bool,
    pub no_derivatives: bool,
}

impl License {
    /// The CC0 1.0 licence: no attribution and no sharing conditions.
    pub fn cc0() -> Self {
        Self {
            id: "cc0-1.0".into(),
            name: "CC0 1.0".into(),
            url: Some("https://creativecommons.org/publicdomain/zero/1.0/".into()),
            attribution_required: false,
            share_alike: false,
            non_commercial: false,
            no_derivatives: false,
        }
    }

    /// A public-domain licence with no attribution conditions.
    pub fn public_domain() -> Self {
        Self {
            id: "public-domain".into(),
            name: "Public domain".into(),
            url: None,
            attribution_required: false,
            share_alike: false,
            non_commercial: false,
            no_derivatives: false,
        }
    }

    /// A licence this build cannot represent; `name` is what the source said.
    pub fn unknown(name: &str) -> Self {
        Self {
            id: "unknown".into(),
            name: name.trim().to_string(),
            url: None,
            attribution_required: true,
            share_alike: false,
            non_commercial: false,
            no_derivatives: false,
        }
    }

    /// Read the licence a Wikimedia Commons file declares. Commons reports a
    /// short name, a licence URL and a longer usage term; any of the three can
    /// be the one that names the licence.
    pub fn from_commons(short: Option<&str>, url: Option<&str>, usage: Option<&str>) -> Self {
        let name = short
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .or_else(|| usage.map(str::trim).filter(|name| !name.is_empty()))
            .unwrap_or("Unknown licence")
            .to_string();
        let url = url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string);
        let mut license = Self::parse(&name, url.as_deref());
        license.name = name;
        license.url = url;
        license
    }

    /// Read the licence a Freesound sound declares. The API reports a label
    /// ("Creative Commons 0", "Attribution", "Attribution NonCommercial") and
    /// older captures report a licence URL instead; both are understood.
    pub fn from_freesound(label: &str) -> Self {
        let name = label.trim().to_string();
        Self::parse(&name, None)
    }

    /// Read a licence from the name and deed URL a source publishes. The name
    /// is kept as the source wrote it, because that is what the badge shows.
    pub fn from_deed(name: &str, url: &str) -> Self {
        let name = name.trim();
        let url = url.trim();
        let mut license = Self::parse(name, Some(url));
        license.name = if name.is_empty() {
            license.name
        } else {
            name.to_string()
        };
        license.url = (!url.is_empty()).then(|| url.to_string());
        license
    }

    /// Read a licence from its deed URL alone (Internet Archive). The name is
    /// rendered from the parsed id, so a badge never shows a raw URL.
    pub fn from_license_url(url: &str) -> Self {
        let url = url.trim();
        let mut license = Self::parse(url, Some(url));
        license.name = id_name(&license.id).unwrap_or_else(|| license.name.clone());
        license.url = (!url.is_empty()).then(|| url.to_string());
        license
    }

    fn parse(name: &str, url: Option<&str>) -> Self {
        let probe = format!(
            "{} {}",
            name.to_lowercase(),
            url.unwrap_or_default().to_lowercase()
        );
        let version = cc_version(&probe);
        let base = |id: String, attribution: bool, share_alike: bool, nc: bool, nd: bool| Self {
            id,
            name: name.to_string(),
            url: url.map(str::to_string),
            attribution_required: attribution,
            share_alike,
            non_commercial: nc,
            no_derivatives: nd,
        };
        if probe.contains("cc0")
            || probe.contains("cc-zero")
            || probe.contains("publicdomain/zero")
            || probe.contains("creative commons 0")
        {
            return Self {
                id: "cc0-1.0".into(),
                name: name.to_string(),
                url: url.map(str::to_string),
                attribution_required: false,
                share_alike: false,
                non_commercial: false,
                no_derivatives: false,
            };
        }
        if probe.contains("public domain") || probe.contains("publicdomain") {
            return Self {
                id: "public-domain".into(),
                name: name.to_string(),
                url: url.map(str::to_string),
                attribution_required: false,
                share_alike: false,
                non_commercial: false,
                no_derivatives: false,
            };
        }
        if probe.contains("gfdl") || probe.contains("gpl") {
            return base("gfdl".into(), true, true, false, false);
        }
        if probe.contains("by-nc-nd") || probe.contains("licenses/by-nc-nd") {
            return base(format!("cc-by-nc-nd-{version}"), true, false, true, true);
        }
        if probe.contains("by-nc-sa")
            || probe.contains("by nc sa")
            || probe.contains("licenses/by-nc-sa")
        {
            return base(format!("cc-by-nc-sa-{version}"), true, true, true, false);
        }
        if probe.contains("by-nc")
            || probe.contains("by nc")
            || probe.contains("noncommercial")
            || probe.contains("licenses/by-nc")
        {
            return base(format!("cc-by-nc-{version}"), true, false, true, false);
        }
        if probe.contains("by-nd")
            || probe.contains("by nd")
            || probe.contains("noderiv")
            || probe.contains("licenses/by-nd")
        {
            return base(format!("cc-by-nd-{version}"), true, false, false, true);
        }
        if probe.contains("by-sa") || probe.contains("by sa") || probe.contains("licenses/by-sa") {
            return base(format!("cc-by-sa-{version}"), true, true, false, false);
        }
        if probe.contains("cc by")
            || probe.contains("cc-by")
            || probe.contains("attribution")
            || probe.contains("licenses/by")
        {
            return base(format!("cc-by-{version}"), true, false, false, false);
        }
        Self::unknown(name)
    }

    /// Whether the licence needs the user's acceptance before the file is
    /// pulled. CC0, public domain, CC BY and CC BY-SA do not.
    pub fn needs_notice(&self) -> bool {
        !self.is_free()
    }

    /// The sentence shown when a pull needs acceptance: the licence, its
    /// restrictions, and the fact that the credit line stays with the file.
    pub fn notice(&self) -> String {
        let mut terms: Vec<&str> = Vec::new();
        if self.non_commercial {
            terms.push("non-commercial");
        }
        if self.no_derivatives {
            terms.push("no derivatives");
        }
        if self.share_alike {
            terms.push("share-alike");
        }
        if terms.is_empty() {
            // A community licence with no CC-style terms: say what it permits
            // rather than claiming it is unclassified.
            terms.push(match self.id.as_str() {
                "splice-preview" => {
                    "the public file is an audition preview; a track released with it needs a Splice subscription licence"
                }
                "sampleswap-community" => {
                    "the uploader dedicated the sound to the public domain, which the site cannot verify for every upload, and the original WAV needs a free account"
                }
                _ => "free for productions, no redistribution of the loops",
            });
        }
        format!(
            "Licence: {}{} ({}). The file keeps this licence and its credit line in the project; how the finished track is used is your call. Send accept_licence: true to download it anyway.",
            self.name,
            self.url_suffix(),
            terms.join(", ")
        )
    }

    /// Whether the licence passes the free-licence filter. Kept for the badge
    /// and for callers that prefer a free result; the pull path uses
    /// [`License::needs_notice`] instead.
    pub fn check(&self) -> Result<(), SampleError> {
        if self.non_commercial {
            return Err(SampleError::refused(
                "license_unsupported",
                format!(
                    "The licence '{}'{} is non-commercial: a project that embeds the sample could not be sold or licensed later, so the marketplace refuses it. Pick a CC0, public-domain, CC BY or CC BY-SA result.",
                    self.name,
                    self.url_suffix()
                ),
            ));
        }
        if self.no_derivatives {
            return Err(SampleError::refused(
                "license_unsupported",
                format!(
                    "The licence '{}'{} does not allow derivatives, and mixing a sample into a track is a derivative. Pick a CC0, public-domain, CC BY or CC BY-SA result.",
                    self.name,
                    self.url_suffix()
                ),
            ));
        }
        if self.id == "gfdl" {
            return Err(SampleError::refused(
                "license_unsupported",
                format!(
                    "The licence '{}'{} is GFDL: it requires shipping the full licence text with the audio, which a project media folder does not carry. Pick a CC0, public-domain, CC BY or CC BY-SA result.",
                    self.name,
                    self.url_suffix()
                ),
            ));
        }
        if !self.is_free() {
            return Err(SampleError::refused(
                "license_unsupported",
                format!(
                    "The licence '{}'{} is not one this build can represent: a sample must be CC0, public domain, CC BY or CC BY-SA to enter a project.",
                    self.name,
                    self.url_suffix()
                ),
            ));
        }
        Ok(())
    }

    fn is_free(&self) -> bool {
        // `cc-by-nc-4.0` starts with `cc-by-`, so the terms decide first.
        if self.non_commercial || self.no_derivatives {
            return false;
        }
        self.id == "cc0-1.0"
            || self.id == "public-domain"
            || self.id.starts_with("cc-by-")
            || self.id.starts_with("cc-by-sa-")
    }

    fn url_suffix(&self) -> String {
        match &self.url {
            Some(url) => format!(" ({url})"),
            None => String::new(),
        }
    }

    /// The one line a project carries while the file is in it. Attribution is
    /// the author, the licence name and the page the sample came from, in that
    /// order, because that is what a CC BY deed asks for.
    pub fn credit(&self, author: &str, source_url: &str) -> String {
        let author = author.trim();
        let author = if author.is_empty() {
            "Unknown author"
        } else {
            author
        };
        let mut line = format!("{author} — {}", self.name);
        if let Some(url) = &self.url {
            line.push_str(&format!(" ({url})"));
        }
        if !source_url.trim().is_empty() {
            line.push_str(&format!(" — {}", source_url.trim()));
        }
        line
    }
}

/// The CC version in a name or licence URL, defaulting to 4.0 when neither
/// carries one.
fn cc_version(probe: &str) -> &'static str {
    for version in ["4.0", "3.0", "2.5", "2.0", "1.0"] {
        if probe.contains(version) {
            return version;
        }
    }
    "4.0"
}

/// A readable name for a parsed licence id, used when only a deed URL was
/// published.
fn id_name(id: &str) -> Option<String> {
    match id {
        "cc0-1.0" => return Some("CC0 1.0".into()),
        "public-domain" => return Some("Public domain".into()),
        "gfdl" => return Some("GFDL".into()),
        _ => {}
    }
    let rest = id.strip_prefix("cc-")?;
    let (kind, version) = rest.rsplit_once('-')?;
    Some(format!("CC {} {version}", kind.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commons_public_domain_file_needs_no_attribution() {
        let license = License::from_commons(Some("Public domain"), None, Some("Public domain"));
        assert_eq!(license.id, "public-domain");
        assert!(!license.attribution_required);
        assert!(license.check().is_ok());
    }

    #[test]
    fn a_commons_cc_by_file_keeps_its_version_and_source_url() {
        let license = License::from_commons(
            Some("CC BY 4.0"),
            Some("https://creativecommons.org/licenses/by/4.0"),
            Some("Creative Commons Attribution 4.0"),
        );
        assert_eq!(license.id, "cc-by-4.0");
        assert!(license.attribution_required);
        assert_eq!(
            license.url.as_deref(),
            Some("https://creativecommons.org/licenses/by/4.0")
        );
        assert!(license.check().is_ok());
    }

    #[test]
    fn share_alike_is_allowed_and_recorded() {
        let license = License::from_commons(
            Some("CC BY-SA 3.0"),
            Some("http://creativecommons.org/licenses/by-sa/3.0/"),
            None,
        );
        assert_eq!(license.id, "cc-by-sa-3.0");
        assert!(license.share_alike);
        assert!(license.check().is_ok());
    }

    #[test]
    fn a_non_commercial_licence_is_refused_with_the_term_named() {
        let license = License::from_commons(
            Some("CC BY-NC 4.0"),
            Some("https://creativecommons.org/licenses/by-nc/4.0/"),
            None,
        );
        let error = license.check().unwrap_err();
        assert_eq!(error.code(), Some("license_unsupported"));
        assert!(error.to_string().contains("non-commercial"), "{error}");
        assert!(
            error.to_string().contains("CC0"),
            "the refusal says what to pick: {error}"
        );
    }

    #[test]
    fn a_no_derivatives_licence_is_refused_with_its_reason() {
        let license = License::from_commons(
            Some("CC BY-ND 4.0"),
            Some("https://creativecommons.org/licenses/by-nd/4.0/"),
            None,
        );
        let error = license.check().unwrap_err();
        assert_eq!(error.code(), Some("license_unsupported"));
        assert!(error.to_string().contains("derivative"), "{error}");
    }

    #[test]
    fn gfdl_and_unknown_licences_are_refused() {
        let gfdl = License::from_commons(Some("GFDL 1.2"), None, None);
        assert_eq!(gfdl.id, "gfdl");
        assert!(gfdl.check().unwrap_err().to_string().contains("GFDL"));

        let unknown = License::from_commons(Some("All rights reserved"), None, None);
        assert_eq!(unknown.id, "unknown");
        let error = unknown.check().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not one this build can represent"),
            "{error}"
        );
    }

    #[test]
    fn the_freesound_labels_map_to_the_same_policy() {
        assert_eq!(License::from_freesound("Creative Commons 0").id, "cc0-1.0");
        assert!(
            License::from_freesound("Creative Commons 0")
                .check()
                .is_ok()
        );
        assert_eq!(License::from_freesound("Attribution").id, "cc-by-4.0");
        assert!(License::from_freesound("Attribution").check().is_ok());
        let nc = License::from_freesound("Attribution NonCommercial");
        assert_eq!(nc.id, "cc-by-nc-4.0");
        assert_eq!(nc.check().unwrap_err().code(), Some("license_unsupported"));
        // Captures that carry a deed URL instead of a label still parse.
        assert_eq!(
            License::from_freesound("http://creativecommons.org/publicdomain/zero/1.0/").id,
            "cc0-1.0"
        );
    }

    #[test]
    fn the_credit_line_names_author_licence_and_source() {
        let license = License::from_commons(
            Some("CC BY 4.0"),
            Some("https://creativecommons.org/licenses/by/4.0"),
            None,
        );
        assert_eq!(
            license.credit(
                "Fixture Artist",
                "https://commons.wikimedia.org/wiki/File:Udu.wav"
            ),
            "Fixture Artist — CC BY 4.0 (https://creativecommons.org/licenses/by/4.0) — https://commons.wikimedia.org/wiki/File:Udu.wav"
        );
        assert!(
            license
                .credit("  ", "https://example.org")
                .starts_with("Unknown author")
        );
    }

    #[test]
    fn a_deed_url_supplies_the_name_and_the_terms() {
        let by = License::from_license_url("http://creativecommons.org/licenses/by/3.0/");
        assert_eq!(by.id, "cc-by-3.0");
        assert_eq!(by.name, "CC BY 3.0");
        assert!(by.attribution_required);
        assert!(by.check().is_ok());

        let nc = License::from_license_url("http://creativecommons.org/licenses/by-nc-sa/4.0/");
        assert_eq!(nc.id, "cc-by-nc-sa-4.0");
        assert_eq!(nc.name, "CC BY-NC-SA 4.0");
        assert_eq!(nc.check().unwrap_err().code(), Some("license_unsupported"));

        let zero = License::from_license_url("https://creativecommons.org/publicdomain/zero/1.0/");
        assert_eq!(zero.id, "cc0-1.0");
        assert_eq!(zero.name, "CC0 1.0");
        // A URL this build cannot represent stays unknown and is refused.
        let unknown = License::from_license_url("https://example.org/licence");
        assert_eq!(unknown.id, "unknown");
        assert_eq!(
            unknown.check().unwrap_err().code(),
            Some("license_unsupported")
        );
    }

    #[test]
    fn a_named_deed_keeps_the_name_the_source_used() {
        let license = License::from_deed(
            "CC BY-SA 4.0",
            "https://creativecommons.org/licenses/by-sa/4.0/",
        );
        assert_eq!(license.id, "cc-by-sa-4.0");
        assert_eq!(license.name, "CC BY-SA 4.0");
        assert_eq!(
            license.url.as_deref(),
            Some("https://creativecommons.org/licenses/by-sa/4.0/")
        );
        assert!(license.share_alike);
        assert!(license.check().is_ok());
    }
}
