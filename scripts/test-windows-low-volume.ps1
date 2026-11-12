# Run from elevated Windows PowerShell 5.1. Only the new image is formatted.
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'The disposable VHD test needs an elevated Windows PowerShell session.'
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("ocp-low-volume-" + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $work
$imagePath = Join-Path $work 'scratch.vhd'
$diskpartScript = Join-Path $work 'create.txt'
$previousVolume = $env:OCP_TEST_LOW_SCRATCH_VOLUME
$attached = $false

try {
    # DiskPart reads the command file in the Windows ANSI code page.
    $command = 'create vdisk file="{0}" maximum=128 type=expandable' -f $imagePath
    $encoding = [Text.Encoding]::GetEncoding(
        [Text.Encoding]::Default.CodePage,
        [Text.EncoderFallback]::ExceptionFallback,
        [Text.DecoderFallback]::ExceptionFallback
    )
    [IO.File]::WriteAllLines($diskpartScript, @($command, 'exit'), $encoding)
    & "$env:SystemRoot\System32\diskpart.exe" /s $diskpartScript
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $imagePath -PathType Leaf)) {
        throw "DiskPart did not create the disposable VHD (exit $LASTEXITCODE)."
    }

    $image = Mount-DiskImage -ImagePath $imagePath -StorageType VHD -PassThru
    $attached = $true
    $disks = @($image | Get-Disk)
    if ($disks.Count -ne 1) {
        throw 'The mounted image did not resolve to exactly one disk.'
    }
    $disk = $disks[0]
    if ($disk.PartitionStyle -ne 'RAW' -or $disk.IsBoot -or $disk.IsSystem -or
        $disk.Size -lt 120MB -or $disk.Size -gt 128MB) {
        throw 'The mounted image did not resolve to the expected blank test disk.'
    }
    $null = Initialize-Disk -InputObject $disk -PartitionStyle MBR
    $partition = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
    $volume = Format-Volume -Partition $partition -FileSystem NTFS -NewFileSystemLabel 'OCP low space' -Confirm:$false
    if ($volume.DriveLetter -notmatch '^[A-Z]$' -or $volume.DriveLetter -ne $partition.DriveLetter -or
        $null -eq $volume.SizeRemaining -or $volume.SizeRemaining -le 0 -or $volume.SizeRemaining -ge 256MB) {
        throw 'The disposable volume must retain its partition drive letter and have between zero and 256 MiB free.'
    }
    $env:OCP_TEST_LOW_SCRATCH_VOLUME = '{0}:\' -f $volume.DriveLetter
    Write-Host ("Testing an isolated NTFS volume with {0} bytes free." -f $volume.SizeRemaining)
    $manifest = Join-Path $PSScriptRoot '..\Cargo.toml'
    & cargo test --manifest-path $manifest -p oc-engine --lib --locked -- clip_pcm::tests::native_low_volume_refuses_spooling --ignored --exact --nocapture |
        Tee-Object -Variable testOutput
    $testExitCode = $LASTEXITCODE
    if ($testExitCode -ne 0) {
        throw "The low-volume test failed (exit $testExitCode)."
    }
    if (-not ($testOutput | Select-String -Pattern 'Spool refused on a volume with [0-9]+ bytes free' -Quiet)) {
        throw 'Cargo did not report a verified low-volume spool refusal.'
    }
} finally {
    $env:OCP_TEST_LOW_SCRATCH_VOLUME = $previousVolume
    if ($attached) {
        $null = Dismount-DiskImage -ImagePath $imagePath -StorageType VHD
    }
    Remove-Item -LiteralPath $work -Recurse -Force
}
