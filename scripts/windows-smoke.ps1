# The Windows smoke of CI: one detached session, end to end, with nothing left
# behind. Run with Windows PowerShell 5.1 or PowerShell 7:
#   powershell -NoProfile -File scripts/windows-smoke.ps1 <path to agent-gossip.exe>
param([Parameter(Mandatory = $true)][string]$Binary)

$ErrorActionPreference = 'Stop'
$ag = (Resolve-Path $Binary).Path

function Fail([string]$why) {
    Write-Host "smoke: FAILED: $why"
    exit 1
}

# The owner stands in for the agent: the daemon follows it.
$owner = Start-Process -FilePath ping -ArgumentList '-n', '600', '127.0.0.1' -PassThru -WindowStyle Hidden
# Where session discovery looks, as the skills put it: <base>\sessions\<owner>.json.
# The engine asks for the folder with SHGetKnownFolderPath; on a runner that is
# %LOCALAPPDATA%.
$state = Join-Path $env:LOCALAPPDATA "agent-gossip\run\sessions\$($owner.Id).json"
try {
    $started = Get-Date
    & $ag create --name ci-smoke --owner-pid $owner.Id --state-file $state | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "create exited $LASTEXITCODE" }
    $launch = ((Get-Date) - $started).TotalSeconds
    Write-Host "smoke: the launcher returned in $([math]::Round($launch, 1)) s"
    if ($launch -gt 10) { Fail "the launcher took $launch s" }

    $out = & $ag ready --state-file $state
    if ($LASTEXITCODE -ne 0) { Fail "ready exited $LASTEXITCODE" }
    $identity = $out | ConvertFrom-Json
    Write-Host "smoke: ready as <$($identity.nickname)>"

    & $ag a2a broadcast --gossip $identity.gossip --nickname $identity.nickname --text 'smoke hello' | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "broadcast exited $LASTEXITCODE" }
    & $ag poll --gossip $identity.gossip --nickname $identity.nickname | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "poll exited $LASTEXITCODE" }

    $out = & $ag leave --session-pid $owner.Id
    if ($LASTEXITCODE -ne 0) { Fail "leave exited $LASTEXITCODE" }
    $report = $out | ConvertFrom-Json
    if ($report.left.Count -ne 1 -or -not $report.left[0].confirmed) {
        Fail "leave did not confirm one daemon: $($report | ConvertTo-Json -Compress)"
    }
    Write-Host 'smoke: leave confirmed the daemon'
}
finally {
    Stop-Process -Id $owner.Id -Force -ErrorAction SilentlyContinue
}

if (Test-Path $state) { Fail "the state file is still there: $state" }
$left = @(Get-Process -Name agent-gossip -ErrorAction SilentlyContinue)
if ($left.Count -ne 0) { Fail "$($left.Count) agent-gossip process(es) still run" }
Write-Host 'smoke: passed'
