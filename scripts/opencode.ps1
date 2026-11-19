# Locate the OpenCode service and call its local HTTP API.
# Dot-source this file; it defines functions and does not execute work.

function Get-OpenCodeCli {
    if ($env:OPENCODE_CLI -and (Test-Path $env:OPENCODE_CLI)) {
        return $env:OPENCODE_CLI
    }
    $command = Get-Command opencode -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    $roots = @(
        (Join-Path $env:APPDATA 'ai.opencode.desktop\cli'),
        (Join-Path $env:LOCALAPPDATA 'Programs\ai.opencode.desktop\cli')
    )
    foreach ($root in $roots) {
        if (-not (Test-Path $root)) { continue }
        $candidate = Get-ChildItem $root -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName 'opencode-cli.exe' } |
            Where-Object { Test-Path $_ } |
            Select-Object -First 1
        if ($candidate) { return $candidate }
    }
    throw "OpenCode CLI not found. Set OPENCODE_CLI to opencode-cli.exe."
}

function Get-OpenCodeService {
    $statePath = Join-Path $env:USERPROFILE '.local\state\opencode\service.json'
    $configPath = Join-Path $env:USERPROFILE '.config\opencode\service.json'
    $url = $null
    $password = $null
    if (Test-Path $statePath) {
        $state = Get-Content $statePath -Raw | ConvertFrom-Json
        $url = $state.url
        $password = $state.password
    }
    if (-not $password -and (Test-Path $configPath)) {
        $config = Get-Content $configPath -Raw | ConvertFrom-Json
        $password = $config.password
    }
    if (-not $url) {
        $cli = Get-OpenCodeCli
        $info = (& $cli api get /api/info 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -eq 0 -and $info) {
            $url = ($info | ConvertFrom-Json).urls[0]
        }
    }
    if (-not $url -or -not $password) {
        throw "OpenCode service URL or password not found."
    }
    return [pscustomobject]@{ Url = $url.TrimEnd('/'); Password = $password }
}

function Invoke-OpenCodeApi {
    param(
        [Parameter(Mandatory = $true)][ValidateSet('get', 'post', 'put', 'delete')][string]$Method,
        [Parameter(Mandatory = $true)][string]$Path,
        [string]$Body
    )
    $service = Get-OpenCodeService
    $pair = "opencode:$($service.Password)"
    $token = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($pair))
    $headers = @{ Authorization = "Basic $token" }
    $uri = "$($service.Url)$Path"
    $arguments = @{
        Uri = $uri
        Method = $Method
        Headers = $headers
        ContentType = 'application/json'
        UseBasicParsing = $true
    }
    if ($PSBoundParameters.ContainsKey('Body') -and $Body) {
        $arguments.Body = [Text.Encoding]::UTF8.GetBytes($Body)
    }
    try {
        $response = Invoke-WebRequest @arguments
        return ($response.Content | Out-String).Trim()
    } catch {
        $detail = $_.Exception.Message
        if ($_.Exception.Response) {
            try {
                $stream = $_.Exception.Response.GetResponseStream()
                $reader = New-Object IO.StreamReader($stream)
                $detail = $reader.ReadToEnd()
            } catch { }
        }
        throw "OpenCode API $Method $Path failed: $detail"
    }
}
