//! Typed request for the read-only project name search.

use crate::state::AppState;
use crate::tool_registry::arguments::typed_arguments;
use oc_dsl::search::{DEFAULT_LIMIT, MAX_LIMIT, MAX_QUERY_CHARS, SearchKind, SearchRequest};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(description = "Object kind to include in the result")]
#[serde(rename_all = "snake_case")]
pub enum ProjectSearchKind {
    Track,
    Preset,
    Pattern,
    Bus,
    Section,
    Marker,
    Effect,
    Phrase,
}

impl From<ProjectSearchKind> for SearchKind {
    fn from(kind: ProjectSearchKind) -> Self {
        match kind {
            ProjectSearchKind::Track => Self::Track,
            ProjectSearchKind::Preset => Self::Preset,
            ProjectSearchKind::Pattern => Self::Pattern,
            ProjectSearchKind::Bus => Self::Bus,
            ProjectSearchKind::Section => Self::Section,
            ProjectSearchKind::Marker => Self::Marker,
            ProjectSearchKind::Effect => Self::Effect,
            ProjectSearchKind::Phrase => Self::Phrase,
        }
    }
}

typed_arguments! {
    /// Search the open project for tracks, presets, patterns, buses, sections, markers, effects and phrases by name, label or type. Case-insensitive. Returns addressable paths.
    pub struct SearchArgs => ProjectSearch {
        #[schemars(length(min = 1, max = 200), description = "Case-insensitive tokens matched against names and labels")]
        pub query: String,
        /// Restrict the result set to these object kinds.
        pub kinds: Option<Vec<ProjectSearchKind>>,
        #[schemars(range(min = 1, max = 50), description = "Maximum hits (default 20)")]
        pub limit: Option<u32>,
    }
}

/// Run a search against the open project.
pub fn execute(state: &AppState, request: SearchArgs) -> Result<Value, String> {
    if request.query.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("query is longer than {MAX_QUERY_CHARS} characters"));
    }
    let limit = request
        .limit
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    let kinds: Option<Vec<SearchKind>> = request
        .kinds
        .map(|kinds| kinds.into_iter().map(Into::into).collect());
    let inner = state.lock_inner();
    let hits = oc_dsl::search::search(
        &inner.project,
        &SearchRequest {
            query: &request.query,
            kinds: kinds.as_deref(),
            limit,
        },
    );
    Ok(json!({ "results": hits, "count": hits.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    fn state(project: &str) -> AppState {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(project).unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
        }
        state
    }

    const PROJECT: &str = r#"@project "Search"
@track bass | instrument: bass/electric_bass
  effects:
    - delay: time=200ms
@track "Lead Guitar" | instrument: guitar/electric
@preset warm
  plugin: "builtin.open_synth"
"#;

    #[test]
    fn a_search_returns_bounded_addressable_hits() {
        let state = state(PROJECT);
        let value = execute(
            &state,
            SearchArgs {
                query: "lead".into(),
                kinds: None,
                limit: Some(1),
            },
        )
        .unwrap();
        assert_eq!(value["count"], 1);
        let hit = &value["results"][0];
        assert_eq!(hit["name"], "Lead Guitar");
        assert_eq!(hit["path"], "track:Lead Guitar");
    }

    #[test]
    fn a_kind_filter_narrows_the_result() {
        let state = state(PROJECT);
        let value = execute(
            &state,
            SearchArgs {
                query: "a".into(),
                kinds: Some(vec![ProjectSearchKind::Effect]),
                limit: None,
            },
        )
        .unwrap();
        assert!(value["count"].as_u64().unwrap() >= 1);
        for hit in value["results"].as_array().unwrap() {
            assert_eq!(hit["kind"], "effect");
        }
    }

    #[test]
    fn an_oversized_query_is_refused() {
        let state = state(PROJECT);
        let error = execute(
            &state,
            SearchArgs {
                query: "x".repeat(MAX_QUERY_CHARS + 1),
                kinds: None,
                limit: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("longer than"), "{error}");
    }
}
