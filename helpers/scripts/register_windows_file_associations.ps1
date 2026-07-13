param(
  [string]$ExecutablePath = "",
  [switch]$Machine,
  [switch]$Unregister
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ExecutablePath)) {
  $ExecutablePath = Join-Path (Resolve-Path ".").Path "target\release\flowstate.exe"
}

$root = if ($Machine) { "Registry::HKEY_LOCAL_MACHINE\Software\Classes" } else { "Registry::HKEY_CURRENT_USER\Software\Classes" }

if ($Unregister) {
  @(
    "$root\.db8",
    "$root\.flowinvite",
    "$root\.docx\OpenWithProgids",
    "$root\Flowstate.db8",
    "$root\Flowstate.invite",
    "$root\Flowstate.docx.import",
    "$root\flowstate"
  ) | ForEach-Object { Remove-Item -LiteralPath $_ -Recurse -Force -ErrorAction SilentlyContinue }
  Write-Host "Removed Flowstate document associations and the flowstate:// protocol"
  exit 0
}

$ExecutablePath = (Resolve-Path -LiteralPath $ExecutablePath).Path

function Set-KeyValue {
  param([string]$Path, [string]$Name, [string]$Value)
  New-Item -Path $Path -Force | Out-Null
  if ($Name -eq "") {
    Set-Item -Path $Path -Value $Value
  } else {
    New-ItemProperty -Path $Path -Name $Name -Value $Value -PropertyType String -Force | Out-Null
  }
}

function Register-Extension {
  param([string]$Extension, [string]$ProgId, [string]$Description, [bool]$MakeDefault)
  if ($MakeDefault) {
    Set-KeyValue -Path "$root\$Extension" -Name "" -Value $ProgId
  } else {
    New-Item -Path "$root\$Extension\OpenWithProgids" -Force | Out-Null
    New-ItemProperty -Path "$root\$Extension\OpenWithProgids" -Name $ProgId -Value "" -PropertyType String -Force | Out-Null
  }
  Set-KeyValue -Path "$root\$ProgId" -Name "" -Value $Description
  Set-KeyValue -Path "$root\$ProgId\shell\open\command" -Name "" -Value "`"$ExecutablePath`" `"%1`""
}

function Register-UrlProtocol {
  param([string]$Scheme, [string]$Description)
  Set-KeyValue -Path "$root\$Scheme" -Name "" -Value $Description
  Set-KeyValue -Path "$root\$Scheme" -Name "URL Protocol" -Value ""
  Set-KeyValue -Path "$root\$Scheme\shell\open\command" -Name "" -Value "`"$ExecutablePath`" `"%1`""
}

Register-Extension -Extension ".db8" -ProgId "Flowstate.db8" -Description "Flowstate Debate Document" -MakeDefault $true
Register-Extension -Extension ".flowinvite" -ProgId "Flowstate.invite" -Description "Flowstate Collaboration Invite" -MakeDefault $true
Register-Extension -Extension ".docx" -ProgId "Flowstate.docx.import" -Description "Microsoft Word Document imported by Flowstate" -MakeDefault $false
Register-UrlProtocol -Scheme "flowstate" -Description "Flowstate collaboration invite"

Write-Host "Registered Flowstate document associations and the flowstate:// invite protocol for $ExecutablePath"
