use std::{path::Path, sync::Mutex};

/// Project path passed on the command line.
pub struct StartupProject(pub Mutex<Option<String>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    Desktop,
    Headless,
}

impl RunMode {
    pub fn from_environment() -> Self {
        if std::env::var_os("OPENCOMPOSE_HEADLESS").is_some() {
            Self::Headless
        } else {
            Self::Desktop
        }
    }

    pub fn is_desktop(self) -> bool {
        self == Self::Desktop
    }

    pub fn configure(self, config: &mut tauri::Config) {
        if !self.is_desktop() {
            config.app.windows.clear();
        }
    }
}

/// Runtime diagnostics switches, compiled in by the `debug-log` / `perf-log`
/// cargo features and turned on here by the command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagOptions {
    pub debug: bool,
    pub perf: bool,
}

impl DiagOptions {
    /// Whether any diagnostics switch is enabled.
    pub fn any(self) -> bool {
        self.debug || self.perf
    }
}

/// Pull `--debug` and `--perf` out of the argument list, leaving the rest.
pub fn parse_diag_options(args: &[String]) -> (DiagOptions, Vec<String>) {
    let mut diag = DiagOptions::default();
    let mut rest = Vec::with_capacity(args.len());
    for arg in args {
        match arg.as_str() {
            "--debug" => diag.debug = true,
            "--perf" => diag.perf = true,
            _ => rest.push(arg.clone()),
        }
    }
    (diag, rest)
}

/// Parse the startup project path.
pub fn parse_path(args: &[String], cwd: &Path) -> Result<Option<String>, String> {
    let (_, args) = parse_diag_options(args);
    let args = if args.first().map(String::as_str) == Some("--") {
        &args[1..]
    } else {
        &args[..]
    };
    match args {
        [] => Ok(None),
        [path] if !path.starts_with('-') => {
            // Linux desktop launchers may supply a file URI rather than a path.
            if path.starts_with("file:") {
                let url = path
                    .parse::<tauri::Url>()
                    .map_err(|error| error.to_string())?;
                return crate::open_events::path_from_url(&url).map(Some);
            }
            let path = cwd.join(path);
            path.to_str()
                .map(|s| Some(s.to_owned()))
                .ok_or_else(|| "Project path is not valid UTF-8".to_owned())
        }
        _ => Err("Usage: opencompose-gui [--debug] [--perf] [PROJECT.ocp]".to_owned()),
    }
}

/// Take the startup project path once.
#[tauri::command]
pub fn take_startup_project(
    state: tauri::State<'_, StartupProject>,
) -> Result<Option<String>, String> {
    state
        .0
        .lock()
        .map(|mut path| path.take())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_configuration_removes_every_webview() {
        let mut config = tauri::Config::default();
        config.app.windows = vec![Default::default(), Default::default()];
        RunMode::Headless.configure(&mut config);
        assert!(config.app.windows.is_empty());
        assert!(!RunMode::Headless.is_desktop());
    }

    #[test]
    fn desktop_configuration_preserves_window_settings() {
        let mut config = tauri::Config::default();
        config.app.windows = vec![Default::default()];
        config.app.windows[0].title = "Session".into();
        RunMode::Desktop.configure(&mut config);
        assert_eq!(config.app.windows.len(), 1);
        assert_eq!(config.app.windows[0].title, "Session");
        assert!(RunMode::Desktop.is_desktop());
    }

    #[test]
    fn desktop_file_urls_are_decoded_before_path_resolution() {
        let file = std::env::temp_dir().join("my song.ocp");
        let url = tauri::Url::from_file_path(&file).unwrap();
        assert_eq!(
            parse_path(&[url.to_string()], Path::new("/")).unwrap(),
            Some(file.to_str().unwrap().into())
        );
        assert!(parse_path(&["file://server/music/a.ocp".into()], Path::new("/")).is_err());
    }

    #[test]
    fn startup_paths() {
        let cwd = Path::new("/work");
        assert_eq!(parse_path(&[], cwd).unwrap(), None);
        assert_eq!(
            parse_path(&["my song.ocp".into()], cwd).unwrap(),
            Some("/work/my song.ocp".into())
        );
        assert_eq!(
            parse_path(&["/tmp/song.ocp".into()], cwd).unwrap(),
            Some("/tmp/song.ocp".into())
        );
        assert!(parse_path(&["a.ocp".into(), "b.ocp".into()], cwd).is_err());
        assert!(parse_path(&["--bad".into()], cwd).is_err());
        // Diagnostics flags are stripped before the path is parsed.
        assert_eq!(
            parse_path(&["--debug".into(), "--perf".into(), "x.ocp".into()], cwd).unwrap(),
            Some("/work/x.ocp".into())
        );
        let (diag, rest) = parse_diag_options(&["--perf".into(), "x.ocp".into(), "--debug".into()]);
        assert!(diag.debug && diag.perf && diag.any());
        assert_eq!(rest, vec!["x.ocp".to_string()]);
    }
}
