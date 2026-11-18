# Installs craft-mcp-hub (crafthub and crafthub-panel) on Windows, or updates it.
#
#   irm https://raw.githubusercontent.com/mayor686/craft-mcp-hub/main/install.ps1 | iex
#
# The programs go to %USERPROFILE%\.crafthub\bin (set CRAFTHUB_INSTALL_DIR for another folder),
# which is added to your PATH, and the control panel gets a Start menu shortcut.

# Run in a block of its own, so nothing it sets is left behind in your session.
& {
  $ErrorActionPreference = 'Stop'
  # Windows PowerShell draws a progress bar that makes downloads many times slower.
  $ProgressPreference = 'SilentlyContinue'
  # Older Windows PowerShell doesn't offer TLS 1.2 by itself, and GitHub accepts nothing older.
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

  if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'craft-mcp-hub is built for 64-bit Windows only.'
  }

  $repo = 'mayor686/craft-mcp-hub'
  $dir = if ($env:CRAFTHUB_INSTALL_DIR) { $env:CRAFTHUB_INSTALL_DIR } else { Join-Path $env:USERPROFILE '.crafthub\bin' }
  $asset = 'crafthub-windows-x86_64.zip'
  $base = "https://github.com/$repo/releases/latest/download"
  $tmp = Join-Path ([IO.Path]::GetTempPath()) ('crafthub-' + [guid]::NewGuid())
  $updating = Test-Path (Join-Path $dir 'crafthub.exe')

  New-Item -ItemType Directory -Force $tmp, $dir | Out-Null
  try {
    Write-Host "Downloading $asset..."
    foreach ($file in $asset, "$asset.sha256") {
      Invoke-WebRequest "$base/$file" -OutFile (Join-Path $tmp $file) -UseBasicParsing
    }

    # Nothing is installed unless the download is the file the release was built with.
    $expected = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw).Trim() -split '\s+')[0]
    $actual = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash
    if (-not $expected -or $expected -ne $actual) {
      throw 'The download does not match its checksum; nothing was installed.'
    }

    Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    foreach ($name in 'crafthub.exe', 'crafthub-panel.exe') {
      $source = Join-Path $tmp $name
      if (-not (Test-Path $source)) { continue }
      $target = Join-Path $dir $name
      if (Test-Path $target) {
        # A running copy can't be overwritten, but it can be moved out of the way.
        $old = "$target.old"
        if (Test-Path $old) { try { Remove-Item $old -Force } catch { $old = "$target.$(Get-Random).old" } }
        Move-Item $target $old -Force
      }
      Copy-Item $source $target
    }
  } finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
  }
  # Copies left from earlier updates go once nothing runs from them any more.
  Get-ChildItem $dir -Filter '*.old' | ForEach-Object { try { Remove-Item $_.FullName -Force -ErrorAction Stop } catch {} }

  # Add the folder to your PATH. It is read and written as stored, so entries written with
  # variables in them, like %USERPROFILE%\bin, stay as they are.
  $environment = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
  try {
    $stored = [string]$environment.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    $entries = @($stored -split ';' | Where-Object { $_ })
    $have = $entries | ForEach-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') }
    if ($have -notcontains $dir.TrimEnd('\')) {
      $kind = if ($stored) { $environment.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
      $environment.SetValue('Path', (($entries + $dir) -join ';'), $kind)
      # Changing any user variable tells running programs to read the environment again, so
      # terminals opened from now on have the new PATH. This one doesn't exist: nothing is lost.
      [Environment]::SetEnvironmentVariable('CRAFTHUB_PATH_CHANGED', $null, 'User')
      $env:Path = "$env:Path;$dir"
    }
  } finally {
    $environment.Close()
  }

  $panel = Join-Path $dir 'crafthub-panel.exe'
  if (Test-Path $panel) {
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'Craft MCP Hub.lnk'))
    $shortcut.TargetPath = $panel
    $shortcut.WorkingDirectory = $dir
    $shortcut.Description = 'Turn the MCP servers of the ArtCraft apps on and off'
    $shortcut.Save()
  }

  Write-Host "Installed in ${dir}: $(& (Join-Path $dir 'crafthub.exe') --version)"
  Write-Host ''
  if ($updating) {
    Write-Host 'Servers that are running keep the previous version until they are restarted:'
    Write-Host '       crafthub restart all    (this closes what their engines have open)'
  } else {
    Write-Host 'Next:  crafthub setup    (adds and turns on every installed ArtCraft app)'
    Write-Host '       crafthub panel    (opens the control panel; also in the Start menu as "Craft MCP Hub")'
  }
}
