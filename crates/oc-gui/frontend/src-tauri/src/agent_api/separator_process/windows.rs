#[cfg(test)]
mod tests {
    use super::super::*;
    use oc_host::child_process::CommandSpec;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    #[test]
    fn separator_windows_stop_kills_the_descendant() {
        let dir = tempfile::Builder::new()
            .prefix("separator with spaces ")
            .tempdir()
            .unwrap();
        let script = dir.path().join("launch.ps1");
        std::fs::write(&script, r#"
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot.Replace("'", "''")
$source = "Set-Content -LiteralPath '$root\started' -Value 'ready'; Start-Sleep -Seconds 2; Set-Content -LiteralPath '$root\escaped' -Value 'late'"
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($source))
$worker = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand',$encoded -NoNewWindow -PassThru
$worker.WaitForExit()
"#).unwrap();
        let mut shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        shell.push(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut command = CommandSpec::new(shell);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script);
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                run(
                    "test separator",
                    command,
                    Duration::from_secs(15),
                    Some(&cancel),
                )
            });
            let marker = dir.path().join("started");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !marker.is_file() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            cancel.store(true, Ordering::Release);
            let result = running.join().unwrap();
            assert!(marker.is_file(), "descendant never started: {result:?}");
            assert_eq!(result.unwrap_err(), "separation cancelled");
        });
        std::thread::sleep(Duration::from_millis(2200));
        assert!(!dir.path().join("escaped").exists());
    }
}
