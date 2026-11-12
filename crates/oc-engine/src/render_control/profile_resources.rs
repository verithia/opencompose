//! Resource admission shared by profile construction and hosted native routing.
use super::*;
use std::path::PathBuf;

const MAX_RESOURCE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * MAX_RESOURCE_BYTES;
const MAX_RESOURCE_DEPTH: usize = 8;
const MAX_ASSIGNMENTS: usize = 4096;

pub(super) fn check_source_limit(source: &str) -> Result<(), ProfileAssignmentError> {
    if source.len() > MAX_RESOURCE_BYTES {
        Err(ProfileAssignmentError::ResourceLimit)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
/// Why a profile assignment could not be resolved.
pub enum ProfileAssignmentError {
    #[error("profile assignment has an invalid value")]
    InvalidValue,
    #[error("profile resource could not be read as bounded regular UTF-8 data")]
    ResourceRead,
    #[error("profile resources exceed the preparation byte or assignment budget")]
    ResourceLimit,
    #[error("profile resource nesting exceeds the supported depth")]
    ResourceDepth,
    #[error("profile resources contain a reference cycle")]
    ResourceCycle,
    #[error("profile manifest has invalid syntax or assignment values")]
    ManifestSyntax,
    #[error("profile tuning resource has invalid syntax or nonfinite values")]
    TuningSyntax,
}

/// Invalid external input remains eligible for authoritative native resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileAssignmentClaim {
    Unrecognized,
    Accepted,
    Invalid(ProfileAssignmentError),
}

/// A candidate name is not an external claim until its actual consumer accepts
/// the value/resource. Native collision checks belong to the host inventory.
pub fn checked_profile_assignment(
    name: &str,
    value: &ParamValue,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> ProfileAssignmentClaim {
    ProfileResources::new(resource_root).claim(name, value, sample_rate)
}

/// Check one bounded authored batch with a shared resource I/O budget.
pub fn checked_profile_assignments<'a>(
    assignments: impl IntoIterator<Item = (&'a str, &'a ParamValue)>,
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<Vec<ProfileAssignmentClaim>, ProfileAssignmentError> {
    let mut resources = ProfileResources::new(resource_root);
    let mut claims = Vec::new();
    for (name, value) in assignments {
        if claims.len() >= MAX_ASSIGNMENTS {
            return Err(ProfileAssignmentError::ResourceLimit);
        }
        claims.push(resources.claim(name, value, sample_rate));
    }
    Ok(claims)
}

pub(super) struct ProfileResources {
    root: Option<PathBuf>,
    stack: Vec<PathBuf>,
    bytes: usize,
    assignments: usize,
    failures: usize,
    last_failure: Option<ProfileAssignmentError>,
}
impl ProfileResources {
    pub(super) fn new(root: Option<&Path>) -> Self {
        Self {
            root: root.map(Path::to_path_buf),
            stack: Vec::new(),
            bytes: 0,
            assignments: 0,
            failures: 0,
            last_failure: None,
        }
    }
    fn claim(
        &mut self,
        name: &str,
        value: &ParamValue,
        sample_rate: u32,
    ) -> ProfileAssignmentClaim {
        let Some(candidate) = classify_profile_assignment(name, value) else {
            return ProfileAssignmentClaim::Unrecognized;
        };
        match candidate.value_status {
            ProfileValueStatus::InvalidValue => {
                ProfileAssignmentClaim::Invalid(ProfileAssignmentError::InvalidValue)
            }
            ProfileValueStatus::ConvertedCandidate => ProfileAssignmentClaim::Accepted,
            ProfileValueStatus::DeferredResource => {
                let ParamValue::String(path) = value else {
                    return ProfileAssignmentClaim::Invalid(ProfileAssignmentError::InvalidValue);
                };
                let result = match candidate.kind {
                    ProfileKeyKind::Manifest => self.manifest(path, sample_rate).map(|_| ()),
                    ProfileKeyKind::ScalaFile => self.scala(path, 60).map(|_| ()),
                    ProfileKeyKind::TunFile => self.tun(path).map(|_| ()),
                    _ => Err(ProfileAssignmentError::InvalidValue),
                };
                match result {
                    Ok(()) => ProfileAssignmentClaim::Accepted,
                    Err(error) => ProfileAssignmentClaim::Invalid(error),
                }
            }
        }
    }
    fn read<T>(
        &mut self,
        path: &Path,
        parse: impl FnOnce(&mut Self, &str, &Path) -> Result<T, ProfileAssignmentError>,
    ) -> Result<T, ProfileAssignmentError> {
        let result = self.read_inner(path, parse);
        if let Err(error) = &result {
            self.failures = self.failures.saturating_add(1);
            self.last_failure = Some(*error);
        }
        result
    }
    fn read_inner<T>(
        &mut self,
        path: &Path,
        parse: impl FnOnce(&mut Self, &str, &Path) -> Result<T, ProfileAssignmentError>,
    ) -> Result<T, ProfileAssignmentError> {
        if self.stack.len() >= MAX_RESOURCE_DEPTH {
            return Err(ProfileAssignmentError::ResourceDepth);
        }
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.stack
                .last()
                .and_then(|parent| parent.parent())
                .or(self.root.as_deref())
                .map_or_else(|| path.to_path_buf(), |root| root.join(path))
        };
        let canonical = resolved
            .canonicalize()
            .map_err(|_| ProfileAssignmentError::ResourceRead)?;
        if self.stack.contains(&canonical) {
            return Err(ProfileAssignmentError::ResourceCycle);
        }
        let remaining = MAX_TOTAL_BYTES
            .checked_sub(self.bytes)
            .filter(|n| *n > 0)
            .ok_or(ProfileAssignmentError::ResourceLimit)?;
        let data = oc_audio::bounded_file::BoundedRegularFile::open(
            &resolved,
            remaining.min(MAX_RESOURCE_BYTES),
        )
        .and_then(|file| file.read())
        .map_err(|_| ProfileAssignmentError::ResourceRead)?;
        self.bytes = self
            .bytes
            .checked_add(data.len())
            .filter(|n| *n <= MAX_TOTAL_BYTES)
            .ok_or(ProfileAssignmentError::ResourceLimit)?;
        let source =
            std::str::from_utf8(&data).map_err(|_| ProfileAssignmentError::ResourceRead)?;
        self.stack.push(canonical);
        let result = parse(self, source, &resolved);
        self.stack.pop();
        result
    }
    pub(super) fn manifest(
        &mut self,
        path: &str,
        rate: u32,
    ) -> Result<InstrumentProfile, ProfileAssignmentError> {
        self.manifest_path(Path::new(path), rate)
    }
    pub(super) fn manifest_path(
        &mut self,
        path: &Path,
        rate: u32,
    ) -> Result<InstrumentProfile, ProfileAssignmentError> {
        self.read(path, |resources, source, resolved| {
            let params = if resolved.extension().and_then(|e| e.to_str()) == Some("toml") {
                let value =
                    toml::from_str(source).map_err(|_| ProfileAssignmentError::ManifestSyntax)?;
                toml_manifest_params(value).map_err(|_| ProfileAssignmentError::ManifestSyntax)?
            } else {
                let value = serde_json::from_str(source)
                    .map_err(|_| ProfileAssignmentError::ManifestSyntax)?;
                json_manifest_params(value).map_err(|_| ProfileAssignmentError::ManifestSyntax)?
            };
            resources.manifest_params(&params, rate)
        })
    }
    pub(super) fn manifest_params(
        &mut self,
        params: &[(String, ParamValue)],
        rate: u32,
    ) -> Result<InstrumentProfile, ProfileAssignmentError> {
        self.assignments = self
            .assignments
            .checked_add(params.len())
            .filter(|n| *n <= MAX_ASSIGNMENTS)
            .ok_or(ProfileAssignmentError::ResourceLimit)?;
        if params.iter().any(|(name, value)| {
            classify_profile_assignment(name, value)
                .is_some_and(|candidate| candidate.value_status == ProfileValueStatus::InvalidValue)
        }) {
            return Err(ProfileAssignmentError::ManifestSyntax);
        }
        let failures = self.failures;
        let profile = InstrumentProfile::from_preset_params_with_resources(params, rate, self);
        if self.failures != failures {
            return Err(self
                .last_failure
                .unwrap_or(ProfileAssignmentError::ManifestSyntax));
        }
        Ok(profile)
    }
    pub(super) fn scala(
        &mut self,
        path: &str,
        root: u8,
    ) -> Result<HashMap<u8, f64>, ProfileAssignmentError> {
        self.read(Path::new(path), |_, source, _| {
            scala_tunings_from_str(source, root).map_err(|_| ProfileAssignmentError::TuningSyntax)
        })
    }
    pub(super) fn tun(&mut self, path: &str) -> Result<HashMap<u8, f64>, ProfileAssignmentError> {
        self.read(Path::new(path), |_, source, _| {
            tun_tunings_from_str(source).map_err(|_| ProfileAssignmentError::TuningSyntax)
        })
    }
}

#[cfg(test)]
mod tests;
