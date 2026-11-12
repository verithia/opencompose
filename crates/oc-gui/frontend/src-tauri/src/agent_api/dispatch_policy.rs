//! Host-selected authority for installation; never deserialized from tool input.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstallPolicy {
    External,
    Panel,
}

impl InstallPolicy {
    pub(super) fn authorize(
        self,
        signed_native_install: impl FnOnce() -> Result<bool, String>,
    ) -> Result<(), String> {
        match self {
            Self::External => Ok(()),
            Self::Panel if signed_native_install()? => Ok(()),
            Self::Panel => Err("refused: the AI panel installs only signed-index, size/checksum-pinned native plugin archives or managed separator runtimes; manual installers and unverified packages are not supported".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn panel_install_policy_requires_host_verified_native_install() {
        let checks = Cell::new(0);
        let rejected = InstallPolicy::Panel.authorize(|| {
            checks.set(checks.get() + 1);
            Ok(false)
        });
        assert!(rejected.unwrap_err().starts_with("refused:"));
        assert_eq!(checks.get(), 1);
        assert!(InstallPolicy::Panel.authorize(|| Ok(true)).is_ok());
        assert_eq!(
            InstallPolicy::Panel.authorize(|| Err("catalog verification failed".into())),
            Err("catalog verification failed".into())
        );
    }

    #[test]
    fn model_arguments_cannot_supply_installation_authority() {
        let tool = crate::tool_registry::panel_tool("plugin_pull").unwrap();
        for marker in [
            serde_json::json!(false),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            let arguments = serde_json::json!({"id": "example", "_panel_install_only": marker});
            assert!(crate::tool_registry::validate_args(&tool, &arguments).is_err());
        }
        assert!(
            crate::tool_registry::validate_args(&tool, &serde_json::json!({"id":"example"}))
                .is_ok()
        );
        assert!(InstallPolicy::Panel.authorize(|| Ok(false)).is_err());
    }

    #[test]
    fn external_install_policy_retains_its_separate_host_authority() {
        assert!(
            InstallPolicy::External
                .authorize(|| panic!("panel policy must not replace external policy"))
                .is_ok()
        );
    }
}
