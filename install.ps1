# tasqx installer for Windows.
#
# Documented forms:
#
#   [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
#   irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1 | iex
#
#   &([scriptblock]::Create((irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1))) -DryRun
#
# iex binds no parameters, so the second form is the one that can carry a
# switch, and every switch also has an environment variable for callers who
# cannot use it.
#
# Windows PowerShell 5.1 is the floor this script is written against: no ternary
# ? :, no ??, no -Parallel, no -SkipHttpErrorCheck. It is still the default shell
# on Windows 11, and it is the shell the one-liner above fails on first when TLS
# 1.2 is not negotiated.
#
# Every byte in this file is ASCII, and that is a constraint rather than an
# accident. The dry-run header contains an em dash, so the file's encoding would
# otherwise decide whether the header prints or mojibakes: 5.1 reads a BOM-less
# file as the system ANSI codepage. A UTF-8 BOM fixes that and was tried, and it
# breaks the first documented form above: 5.1's `irm` passes the BOM through, so
# U+FEFF becomes the first character `iex` and [scriptblock]::Create parse, the
# param block never runs, and the one-liner quietly installs instead of doing
# the dry run it was asked for. The em dash is therefore built from its code
# point where it is printed, and no byte here depends on being decoded right.

param(
    [switch]$DryRun,
    [switch]$Uninstall,
    [switch]$Completions,
    [switch]$Help
)

# TLS 1.2 is set here as well as in the documented prelude. The prelude only got
# irm as far as fetching this file; the requests below are this script's own, and
# on a 5.1 process whose SecurityProtocol excludes Tls12 they fail with "Could
# not create SSL/TLS secure channel" -- an error naming neither tasqx nor the
# cause. Setting it twice is harmless. Assuming it was set once is a failure on
# exactly the machines this script exists to serve. It is OR-ed in rather than
# assigned, so a shell that already negotiated TLS 1.3 keeps it.
$TasqxTls = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
[Net.ServicePointManager]::SecurityProtocol = $TasqxTls

$TasqxRepoUrl = 'https://github.com/dimitritholen/tasqx'
$TasqxLatestUrl = "$TasqxRepoUrl/releases/latest"
$TasqxTagUrlPrefix = "$TasqxRepoUrl/releases/tag/"
$TasqxRawUrl = 'https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1'

function Write-Err {
    param([string]$Text)
    [Console]::Error.WriteLine($Text)
}

# Symmetric with Write-Err, and it exists for the functions below that print a
# line AND return a value. Write-Output puts its argument on the calling
# function's own pipeline, so `if (Invoke-CompletionUninstall ...)` would test
# an array of [the message, the status] and the message would never reach the
# terminal at all. Observed exactly that: -Completions ran, edited the profile,
# and printed nothing.
function Write-Out {
    param([string]$Text)
    [Console]::Out.WriteLine($Text)
}

# A TASQX_* switch counts as set unless it holds nothing, or one of the words
# somebody reaching for "off" would actually type. Documented in -Help, because
# a truthiness rule nobody can read is a truthiness rule nobody can rely on.
function Test-EnvSwitch {
    param([string]$Name)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ($null -eq $value) { return $false }
    $value = $value.Trim()
    if ($value.Length -eq 0) { return $false }
    $off = @('0', 'false', 'no', 'off')
    if ($off -contains $value.ToLowerInvariant()) { return $false }
    return $true
}

function Show-Help {
    Write-Output @"
tasqx installer for Windows

Usage
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
  irm $TasqxRawUrl | iex

  &([scriptblock]::Create((irm $TasqxRawUrl))) -DryRun

Parameters
  -DryRun        Print what the other switches would do, and change nothing.
                 It wins over -Uninstall and -Completions rather than racing
                 them.
  -Uninstall     Remove an installed tasqx.
  -Completions   Install shell completions only.
  -Help          Print this text.

Environment
  iex binds no parameters, so every switch has an equivalent variable:
  TASQX_DRY_RUN, TASQX_UNINSTALL, TASQX_COMPLETIONS, TASQX_HELP.
  A switch variable counts as set unless it is empty, 0, false, no or off.

  TASQX_VERSION  Release tag to install, with or without the leading v.
                 Default: whatever $TasqxLatestUrl redirects to.
  TASQX_INSTALL  Destination directory.
                 Default: %LOCALAPPDATA%\Programs\tasqx\bin
"@
}

# An x64 PowerShell process on an ARM64 machine reports AMD64 in
# PROCESSOR_ARCHITECTURE and the truth in PROCESSOR_ARCHITEW6432, so the WOW64
# variable is read first and the plain one is the fallback.
function Get-HostArchitecture {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if ($null -eq $arch -or $arch.Trim().Length -eq 0) {
        $arch = $env:PROCESSOR_ARCHITECTURE
    }
    if ($null -eq $arch) { return '' }
    return $arch.Trim()
}

function Show-UnmappedPlatform {
    param([string]$Architecture)
    $shown = $Architecture
    if ($shown.Length -eq 0) { $shown = 'unknown' }
    Write-Err "tasqx has no prebuilt binary for Windows/$shown."
    Write-Err 'Prebuilt targets: x86_64-linux, aarch64-macos, x86_64-macos, x86_64-windows.'
    Write-Err "Build from source instead: cargo install --git $TasqxRepoUrl tasqx-cli"
}

# A header value arrives as a string from Windows PowerShell 5.1, as a Uri off
# the HttpResponseMessage PowerShell 7 hands back, and as a one-element
# collection of strings from 7's header dictionary. All three collapse here, so
# the four checks below run against one string rather than three shapes.
function ConvertTo-HeaderString {
    param($Value)
    if ($null -eq $Value) { return '' }
    if ($Value -is [string]) { return $Value }
    if ($Value -is [uri]) { return $Value.AbsoluteUri }
    if ($Value -is [System.Collections.IEnumerable]) {
        foreach ($item in $Value) {
            $text = ConvertTo-HeaderString $item
            if ($text.Length -gt 0) { return $text }
        }
        return ''
    }
    return [string]$Value
}

function Get-RedirectLocation {
    param($Response)
    if ($null -eq $Response) { return '' }
    $headers = $Response.Headers
    if ($null -eq $headers) { return '' }
    $value = $null
    try { $value = $headers.Location } catch { $value = $null }
    $text = ConvertTo-HeaderString $value
    if ($text.Length -eq 0) {
        try { $value = $headers['Location'] } catch { $value = $null }
        $text = ConvertTo-HeaderString $value
    }
    return $text
}

function Test-ReleaseTag {
    param([string]$Tag)
    return ($Tag -cmatch '^v\d+\.\d+\.\d+$')
}

# The newest tag comes from the Location header of /releases/latest, not from
# the JSON API: that API is rate-limited to 60 requests an hour per
# unauthenticated IP and fails invisibly behind shared NAT.
#
# Invoke-WebRequest follows redirects by default, so -MaximumRedirection 0 is
# what makes the header readable at all -- reading the final URL after the
# redirect has been followed cannot tell a tag redirect from a captive portal's.
#
# The two shells then disagree about where the 302 ends up, and neither is
# guessable from the other:
#
#   PowerShell 7   throws HttpResponseException, and the whole response --
#                  Location included -- hangs off $_.Exception.Response.
#   Windows 5.1    throws a bare InvalidOperationException ("Operation is not
#                  valid due to the current state of the object") whose Response
#                  is null. The 302 is only reachable by asking again with a
#                  non-terminating error action, which *returns* it.
#
# So the request is made the way 7 answers, and if that yields nothing, again
# the way 5.1 answers. One extra round trip on 5.1 buys a header that is read
# rather than inferred.
function Resolve-ReleaseTag {
    $location = ''
    $reason = ''
    try {
        $response = Invoke-WebRequest -Uri $TasqxLatestUrl -MaximumRedirection 0 -UseBasicParsing -ErrorAction Stop
        $location = Get-RedirectLocation $response
    } catch {
        $reason = $_.Exception.Message
        $location = Get-RedirectLocation $_.Exception.Response
    }
    if ($location.Length -eq 0) {
        $fallback = $null
        try {
            $fallback = Invoke-WebRequest -Uri $TasqxLatestUrl -MaximumRedirection 0 -UseBasicParsing -ErrorAction SilentlyContinue
        } catch {
            $fallback = $null
        }
        $location = Get-RedirectLocation $fallback
    }

    # Check 1 -- the fetch is checked. A failed fetch must not leave an empty
    # version behind, which would build a URL like .../download//tasqx--x86_64...
    if ($location.Length -eq 0) {
        Write-Err "tasqx installer: could not read the redirect from $TasqxLatestUrl."
        if ($reason.Length -gt 0) { Write-Err "  $reason" }
        Write-Err '  Set TASQX_VERSION to a tag such as v0.3.0 to skip this lookup.'
        return $null
    }

    # Check 2 -- HTTP header lines are CRLF-terminated. A carriage return left
    # inside the URL produces a 404 naming neither the file nor the cause.
    $location = $location.Trim()
    if ($location -match '[\x00-\x1f]') {
        Write-Err "tasqx installer: the redirect from $TasqxLatestUrl contains a control character."
        return $null
    }

    # Check 3 -- a captive portal answering 200 with HTML, a 429, and a
    # repository with no releases all arrive here. Without this the "version"
    # becomes a word like releases.
    if (-not $location.StartsWith($TasqxTagUrlPrefix, [StringComparison]::Ordinal)) {
        Write-Err 'tasqx installer: the redirect from releases/latest did not point at a tag.'
        Write-Err "  expected a URL starting with $TasqxTagUrlPrefix"
        Write-Err "  got $location"
        return $null
    }

    # Check 4 -- and the tail of that URL still has to look like a version.
    $tag = $location.Substring($TasqxTagUrlPrefix.Length)
    if (-not (Test-ReleaseTag $tag)) {
        Write-Err "tasqx installer: '$tag' is not a release tag of the form v1.2.3."
        return $null
    }
    return $tag
}

# Windows names "somebody else has this file open" with two different codes, and
# which one arrives depends on what was attempted rather than on what is wrong:
#
#   0x80070020 ERROR_SHARING_VIOLATION   another handle held without
#                                        FILE_SHARE_DELETE -- a rename or an
#                                        open-for-write hits this
#   0x80070005 ERROR_ACCESS_DENIED       the loader's image section for a
#                                        *running* .exe, which refuses deletion
#
# Both mean the same thing to the person running this, so both route to the same
# instruction. The code is never on the outermost exception: PowerShell wraps
# anything a .NET method throws in a MethodInvocationException whose own HResult
# is 0x80131501, while a cmdlet such as Move-Item throws the IOException bare.
# Walking the chain covers both shapes; reading only $_.Exception covers one.
function Test-LockHResult {
    param($Exception)
    $current = $Exception
    while ($null -ne $current) {
        $code = 0
        try { $code = [int]$current.HResult } catch { $code = 0 }
        if ($code -eq -2147024864 -or $code -eq -2147024891) { return $true }
        $current = $current.InnerException
    }
    return $false
}

function Show-LockedInstruction {
    param([string]$Path)
    Write-Err "tasqx installer: $Path is open in another process and cannot be replaced."
    Write-Err '  tasqx is a program that runs, so this is an ordinary upgrade, not a broken machine:'
    Write-Err '    - stop any running `tasqx daemon` (Ctrl-C in its window, or: Get-Process tasqx | Stop-Process)'
    Write-Err '    - close the MCP client that launches tasqx (Claude Code, or your editor)'
    Write-Err '  Then run this installer again. Nothing has been changed.'
}

# One attempt, then a clear error. Retrying is deliberately not done here: a
# second attempt against a 404 or a captive portal takes twice as long to tell
# the user the same thing.
#
# $_.Exception.Response is deliberately never read. On Windows PowerShell 5.1 it
# is null for whole families of failure, and reading it turns a download error
# with a usable message into a null-reference error with none.
#
# The write is done by WebClient rather than by `Invoke-WebRequest -OutFile`,
# and that is the same wildcard trap that makes `Expand-Archive -Path`
# unusable, arriving one step earlier. -OutFile has no -LiteralPath twin and
# glob-expands what it is given, so a TEMP directory containing `[` -- which is
# a legal directory name -- fails with "Unable to find the specified file.",
# naming neither the bracket nor the path. Escaping the metacharacters was
# tried: 5.1 then writes the escape characters through into the filename and
# fails on the parent instead. WebClient takes a path and treats it as a path.
# It honours the ServicePointManager TLS setting made at the top of this file.
function Save-RemoteFile {
    param([string]$Uri, [string]$OutFile, [string]$What)
    $client = $null
    try {
        $client = New-Object System.Net.WebClient
        $client.DownloadFile($Uri, $OutFile)
    } catch {
        Write-Err "tasqx installer: could not download the $What."
        Write-Err "  $Uri"
        # A .NET method call arrives wrapped in a MethodInvocationException
        # whose message repeats the call signature before saying anything
        # useful. The inner one is the WebException.
        $detail = $_.Exception.Message
        if ($null -ne $_.Exception.InnerException) { $detail = $_.Exception.InnerException.Message }
        Write-Err "  $detail"
        return $false
    } finally {
        if ($null -ne $client) { $client.Dispose() }
    }
    return $true
}

# release.yml:172 writes the .sha256 with Set-Content, which terminates the line
# with CRLF -- the Unix side's shasum output does not. Get-Content -Raw would
# leave that carriage return attached to the hash, and the comparison would then
# fail 100% of the time between two sums that look identical in the error
# message. One line, first whitespace-separated field, no line terminator
# anywhere near the value.
function Get-PublishedSha256 {
    param([string]$Path)
    $line = Get-Content -LiteralPath $Path -First 1
    if ($null -eq $line) { return '' }
    $fields = ([string]$line).Trim() -split '\s+'
    if ($fields.Count -eq 0) { return '' }
    return $fields[0]
}

function Get-NormalizedPath {
    param([string]$Path)
    if ($null -eq $Path -or $Path.Length -eq 0) { return '' }
    try {
        return ([IO.Path]::GetFullPath($Path)).TrimEnd('\').ToLowerInvariant()
    } catch {
        return $Path.TrimEnd('\').ToLowerInvariant()
    }
}

# The shadowed-PATH warning has to name the other binary's version, so the other
# binary is run to get it. Guessing the number, or printing the tag this
# installer just wrote, would produce a line that reads true and is not.
function Get-BinaryVersion {
    param([string]$Path)
    $text = ''
    try {
        $text = [string](& $Path --version 2>&1 | Select-Object -First 1)
    } catch {
        $text = ''
    }
    $found = [regex]::Match($text, '\d+\.\d+\.\d+')
    if ($found.Success) { return $found.Value }
    return 'unknown version'
}

# An upgrade over a running tasqx renames the old exe aside rather than failing,
# and that rename can outlive the process that made it necessary. The sweep is
# here, at the start of the next install, rather than at the end of the one that
# created it: by then the daemon holding the previous copy has usually gone.
#
# The .new- name is swept too. Its own failure paths delete it, but an install
# killed between the copy and the rename cannot run them, and an orphan nobody
# ever removes is how an install directory grows a 7MB file per interrupted run.
function Invoke-StaleBinarySweep {
    param([string]$Directory)
    if (-not (Test-Path -LiteralPath $Directory)) { return }
    foreach ($pattern in @('tasqx.exe.old-*', 'tasqx.exe.new-*')) {
        $stale = @(Get-ChildItem -LiteralPath $Directory -Filter $pattern -File -ErrorAction SilentlyContinue)
        foreach ($item in $stale) {
            Remove-Item -LiteralPath $item.FullName -Force -ErrorAction SilentlyContinue
        }
    }
}

# Windows refuses to overwrite a running .exe, and tasqx is a program that runs:
# `tasqx daemon` and the MCP server the user's client launches both hold this
# file open, so an upgrade -- the common case -- lands here with the file busy.
# Windows does allow a running image to be *renamed*, so the old copy is moved
# to a sibling name, the new one takes its place, and the old one is deleted if
# it can be and swept on a later run if it cannot.
#
# Returns $true on success. On failure it has already said why.
function Install-Binary {
    param([string]$Source, [string]$Destination)

    $directory = Split-Path -Parent $Destination
    if (-not (Test-Path -LiteralPath $directory)) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }
    Invoke-StaleBinarySweep -Directory $directory

    # Staged inside the destination directory, never across volumes, so the move
    # that follows is a rename: an interrupted copy must not leave a truncated
    # exe where a working one used to be.
    $staged = Join-Path $directory ('tasqx.exe.new-' + [Guid]::NewGuid().ToString('n'))
    try {
        Copy-Item -LiteralPath $Source -Destination $staged -Force -ErrorAction Stop
    } catch {
        Write-Err "tasqx installer: could not write into $directory."
        Write-Err "  $($_.Exception.Message)"
        return $false
    }

    $aside = ''
    if (Test-Path -LiteralPath $Destination) {
        $aside = Join-Path $directory ('tasqx.exe.old-' + [Guid]::NewGuid().ToString('n'))
        try {
            Move-Item -LiteralPath $Destination -Destination $aside -ErrorAction Stop
        } catch {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
            if (Test-LockHResult $_.Exception) {
                Show-LockedInstruction $Destination
            } else {
                Write-Err "tasqx installer: could not move the existing $Destination out of the way."
                Write-Err "  $($_.Exception.Message)"
            }
            return $false
        }
    }

    try {
        Move-Item -LiteralPath $staged -Destination $Destination -ErrorAction Stop
    } catch {
        # The old binary is already aside, so a bare failure here would leave
        # the destination with no tasqx at all -- worse than the upgrade not
        # happening. Put it back before saying anything.
        if ($aside.Length -gt 0 -and (Test-Path -LiteralPath $aside)) {
            Move-Item -LiteralPath $aside -Destination $Destination -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        if (Test-LockHResult $_.Exception) {
            Show-LockedInstruction $Destination
        } else {
            Write-Err "tasqx installer: could not move the new binary into place at $Destination."
            Write-Err "  $($_.Exception.Message)"
        }
        return $false
    }

    if ($aside.Length -gt 0) {
        Remove-Item -LiteralPath $aside -Force -ErrorAction SilentlyContinue
    }
    return $true
}

# ---- the user PATH ---------------------------------------------------------
#
# The obvious way to do this corrupts the user's PATH, silently and
# permanently, so none of it is done the obvious way.
#
# Windows stores the user Path as a REG_EXPAND_SZ value, and real ones contain
# placeholders: %USERPROFILE%\.dotnet\tools is what winget and the .NET SDK
# leave behind. [Environment]::GetEnvironmentVariable('Path','User') EXPANDS
# those before handing the string over, so read-modify-write through it
# replaces every placeholder with a literal path AND downgrades the value to
# REG_SZ, after which placeholders stop expanding at all. Nobody notices for
# months, and by then the original text is gone, so -Uninstall cannot put it
# back. The registry is therefore read and written directly, with
# DoNotExpandEnvironmentNames on the way in and ExpandString on the way out.
#
# `setx` is not used either: it truncates at 1024 characters without saying so.

function Get-RawUserPath {
    # $null means "no value there, or it could not be read" -- which -Uninstall
    # treats as a reason to write nothing at all, rather than as an empty PATH.
    $key = $null
    try {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
        if ($null -eq $key) { return $null }
        $value = $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $value) { return $null }
        return [string]$value
    } catch {
        return $null
    } finally {
        if ($null -ne $key) { $key.Close() }
    }
}

function Write-RawUserPath {
    param([string]$Value)
    $key = $null
    try {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        if ($null -eq $key) { return $false }
        # ExpandString, always. Writing this as the default String kind is the
        # second half of the corruption described above: the placeholders that
        # survived the read stop being placeholders on the way back in.
        $key.SetValue('Path', $Value, [Microsoft.Win32.RegistryValueKind]::ExpandString)
        return $true
    } catch {
        Write-Err 'tasqx installer: could not write your user PATH.'
        Write-Err "  $($_.Exception.Message)"
        return $false
    } finally {
        if ($null -ne $key) { $key.Close() }
    }
}

# HWND_BROADCAST + WM_SETTINGCHANGE("Environment") is what Explorer and every
# shell launched afterwards listen for. Without it the new entry is in the
# registry but no newly launched process sees it until the next logon, which
# reads to the user as "the installer lied".
#
# SMTO_ABORTIFHUNG with a short timeout, because HWND_BROADCAST reaches every
# top-level window on the desktop and one hung application must not hang an
# installer.
$TasqxBroadcastSource = @'
using System;
using System.Runtime.InteropServices;
public static class TasqxBroadcast {
    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
    private static extern IntPtr SendMessageTimeout(
        IntPtr hWnd, uint Msg, IntPtr wParam, string lParam,
        uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
    public static void Notify() {
        UIntPtr unused;
        SendMessageTimeout((IntPtr)0xffff, 0x001A, IntPtr.Zero, "Environment", 0x0002, 2000, out unused);
    }
}
'@

function Publish-EnvironmentChange {
    try {
        # Add-Type throws on a second call for a type it already compiled, and
        # this script can reach here twice in one session under -Uninstall.
        if ($null -eq ('TasqxBroadcast' -as [type])) {
            Add-Type -TypeDefinition $TasqxBroadcastSource -ErrorAction Stop | Out-Null
        }
        [TasqxBroadcast]::Notify()
    } catch {
        # Best effort and deliberately not fatal: the registry write is the
        # thing that had to succeed. A shell started after the next logon picks
        # the entry up whether or not this message was delivered.
        Write-Err "note: could not broadcast the environment change ($($_.Exception.Message)). A new logon will still pick the PATH up."
    }
}

# Windows paths are case-insensitive and a trailing backslash means nothing, so
# two spellings of the same directory are the same entry. Getting this wrong
# makes the installer add a second entry every time it runs.
function Get-ComparablePathEntry {
    param([string]$Entry)
    if ($null -eq $Entry) { return '' }
    return $Entry.Trim().TrimEnd('\').ToLowerInvariant()
}

# The index of $Entry among the ';'-separated segments of $Current, or -1.
# The index rather than a boolean, because the removal below needs to know
# WHICH segment it is in order to cut it out by offset.
function Find-PathEntry {
    param([string]$Current, [string]$Entry)
    if ($null -eq $Current -or $Current.Length -eq 0) { return -1 }
    $wanted = Get-ComparablePathEntry $Entry
    if ($wanted.Length -eq 0) { return -1 }
    $parts = $Current.Split(';')
    for ($i = 0; $i -lt $parts.Length; $i++) {
        if ((Get-ComparablePathEntry $parts[$i]) -eq $wanted) { return $i }
    }
    return -1
}

# Append, and choose the form that Get-PathTextWithout can undo byte for byte.
#
# A PATH ending in ';' is normal -- the one on the machine this was written
# against does -- and `$current + ';' + $entry` would quietly eat that trailing
# separator on the way back out, so the uninstall would not restore the string
# it found. Appending `$entry + ';'` after an existing trailing ';' keeps the
# file's own convention and round-trips exactly.
function Get-PathTextWith {
    param([string]$Current, [string]$Entry)
    if ($null -eq $Current) { $Current = '' }
    if ($Current.Length -eq 0) { return $Entry }
    if ($Current.EndsWith(';')) { return $Current + $Entry + ';' }
    return $Current + ';' + $Entry
}

# Cut one segment out by byte offset, together with exactly one adjacent
# separator, and leave every other byte where it was.
#
# Splitting on ';' and re-joining the survivors would be shorter and is wrong:
# it silently normalises away doubled separators, a trailing ';' and any
# whitespace the user has in there, so an uninstall would hand back a PATH that
# is not the one it was given. The spec's rule -- remove the exact entry, never
# rebuild the string from parts -- is this function.
function Get-PathTextWithout {
    param([string]$Current, [int]$Index)
    $parts = $Current.Split(';')
    $start = 0
    for ($i = 0; $i -lt $Index; $i++) { $start += $parts[$i].Length + 1 }
    $length = $parts[$Index].Length
    if ($Index -gt 0) {
        $start -= 1
        $length += 1
    } elseif ($parts.Length -gt 1) {
        $length += 1
    }
    return $Current.Remove($start, $length)
}

# Returns 'present', 'added' or 'failed'. Says nothing itself: the caller
# prints after the install report, so the lines come out in the order they
# happened.
function Add-UserPathEntry {
    param([string]$Entry)
    $current = Get-RawUserPath
    if ($null -eq $current) { $current = '' }
    if ((Find-PathEntry -Current $current -Entry $Entry) -ge 0) { return 'present' }

    # Read again here, immediately before composing the value that gets
    # written. Two installers running at once is not a race this can win --
    # there is no lock on HKCU\Environment that anybody else takes -- but the
    # window in which the other one's append is overwritten is narrowed to the
    # two statements below rather than spanning everything since the first read.
    $current = Get-RawUserPath
    if ($null -eq $current) { $current = '' }
    if ((Find-PathEntry -Current $current -Entry $Entry) -ge 0) { return 'present' }

    if (-not (Write-RawUserPath -Value (Get-PathTextWith -Current $current -Entry $Entry))) {
        return 'failed'
    }
    Publish-EnvironmentChange
    return 'added'
}

# Returns 'removed', 'absent' or 'aborted'.
function Revoke-UserPathEntry {
    param([string]$Entry)
    $current = Get-RawUserPath
    if ($null -eq $current -or $current.Length -eq 0) {
        # Nothing is computed and nothing is written. A PATH that reads back
        # empty is either a value that is not there or a read that failed, and
        # in both cases writing what this function thinks the answer should be
        # is how a user's PATH gets replaced by one entry.
        Write-Err 'tasqx installer: your user PATH read back empty, so it was left untouched.'
        Write-Err "  Remove this entry by hand if it is there: $Entry"
        return 'aborted'
    }
    if ((Find-PathEntry -Current $current -Entry $Entry) -lt 0) { return 'absent' }

    $current = Get-RawUserPath
    if ($null -eq $current -or $current.Length -eq 0) {
        Write-Err 'tasqx installer: your user PATH read back empty, so it was left untouched.'
        Write-Err "  Remove this entry by hand if it is there: $Entry"
        return 'aborted'
    }
    $index = Find-PathEntry -Current $current -Entry $Entry
    if ($index -lt 0) { return 'absent' }

    if (-not (Write-RawUserPath -Value (Get-PathTextWithout -Current $current -Index $index))) {
        return 'aborted'
    }
    Publish-EnvironmentChange
    return 'removed'
}

# The registry write is for the next shell; this is for this one, and it exists
# so Show-InstallReport below tells the truth. Without it every fresh install
# prints "Added to your user PATH" and "warning: ... is not on your PATH" one
# after the other, because Get-Command searches $env:Path and the process that
# is running was started before the entry existed.
#
# Appended, never prepended, so the shadow check answers the question a new
# shell would answer rather than a friendlier one.
function Add-SessionPathEntry {
    param([string]$Entry)
    $current = $env:Path
    if ($null -eq $current) { $current = '' }
    if ((Find-PathEntry -Current $current -Entry $Entry) -ge 0) { return }
    $env:Path = Get-PathTextWith -Current $current -Entry $Entry
}

# ---- completions -----------------------------------------------------------
#
# `tasqx completions --install -y` refuses on every Windows machine, and the
# refusal is right: target_path returns Target::OnlyTheHostKnows
# (crates/tasqx-cli/src/complete/install.rs:459-465) because $PROFILE is a
# PowerShell variable rather than an environment variable, and its value
# differs between Windows PowerShell, PowerShell 7 and the ISE. Only a running
# PowerShell can expand it, which is what this script is. The refusal text
# names the working form, and it is the form used below.
#
# -y is not optional either: install.rs:994 withholds consent when stdin is not
# a terminal, and under `irm | iex` stdin is the script.

function Invoke-CompletionInstall {
    param([string]$Exe, [string]$ProfilePath)
    Write-Out "running: $Exe completions powershell --install --profile $ProfilePath -y"
    $output = & $Exe completions powershell --install --profile $ProfilePath -y 2>&1
    $code = $LASTEXITCODE
    foreach ($line in @($output)) { Write-Out "  $line" }
    if ($code -ne 0) {
        # A warning, and exit 0 at the call site. The binary is installed and
        # works; Tab completion is the thing that did not happen, and failing
        # the whole run here would report a broken install that is not broken.
        Write-Err "warning: could not install PowerShell completions (exit $code)."
        Write-Err "  Run it yourself with: $Exe completions powershell --install --profile `$PROFILE"
        return $false
    }
    return $true
}

# Exit 4 is `not_found`, which here means "there was no block", and that is the
# ordinary case on an uninstall. It is not an error and it is not reported: D33
# makes the CLI refuse to answer ok when it changed nothing, and this caller is
# the one that asked whether there was anything to change.
function Invoke-CompletionUninstall {
    param([string]$Exe, [string]$ProfilePath)
    $output = & $Exe completions powershell --uninstall --profile $ProfilePath -y 2>&1
    $code = $LASTEXITCODE
    if ($code -eq 0) {
        Write-Out "removed the tasqx completion block from $ProfilePath"
        return $true
    }
    if ($code -ne 4) {
        Write-Err "warning: could not remove the completion block from $ProfilePath (exit $code)."
        foreach ($line in @($output)) { Write-Err "  $line" }
    }
    return $false
}

# ---- where the binary lives ------------------------------------------------
#
# Both -Uninstall and the install path need this, and they must not disagree:
# an uninstall that computes a different directory removes nothing and reports
# success.
function Get-DefaultInstallDirectory {
    $localAppData = $env:LOCALAPPDATA
    if ($null -eq $localAppData -or $localAppData.Trim().Length -eq 0) { return '' }
    $programs = Join-Path $localAppData.Trim() 'Programs'
    return Join-Path (Join-Path $programs 'tasqx') 'bin'
}

function Resolve-InstallDirectory {
    $installDir = $env:TASQX_INSTALL
    if ($null -ne $installDir -and $installDir.Trim().Length -gt 0) {
        return $installDir.Trim()
    }
    return Get-DefaultInstallDirectory
}

# What the user's shell will actually run, asked of the shell rather than
# inferred from the PATH string. A cargo-installed tasqx in ~\.cargo\bin is the
# likely shadow -- CLAUDE.md tells every contributor to create one -- and an
# installer that reports success while a different binary answers
# `tasqx --version` is the worst outcome available here: the user concludes the
# installer is broken, or does not notice.
#
# Three branches and no fourth. The "not on your PATH" branch is reachable only
# when the PATH write above failed or was refused; the caller adds the install
# directory to this process's $env:Path first, so the usual answer to
# Get-Command is the binary that was just installed.
function Show-InstallReport {
    param([string]$Tag, [string]$InstallDir, [string]$BinaryPath)

    Write-Output "tasqx $Tag installed to $BinaryPath"

    $resolved = ''
    $command = @(Get-Command tasqx -CommandType Application -ErrorAction SilentlyContinue) | Select-Object -First 1
    if ($null -ne $command) { $resolved = [string]$command.Source }

    if ($resolved.Length -eq 0) {
        Write-Err "warning: $InstallDir is not on your PATH."
        return
    }
    if ((Get-NormalizedPath $resolved) -eq (Get-NormalizedPath $BinaryPath)) { return }
    $otherVersion = Get-BinaryVersion $resolved
    Write-Err "warning: 'tasqx' on your PATH resolves to $resolved ($otherVersion), not the one just installed."
}

# The three things the install did, undone in the one order that works.
#
# The completion block goes first because removing it means RUNNING the binary,
# and step 2 deletes the binary. Reversing those two leaves a block in the
# user's profile that nothing on the machine can now take out.
#
# The store is never touched. TASQX_DB and the tasks in it are the user's data,
# not installer state, and an uninstaller that removes a task database because
# it also wrote a file into LOCALAPPDATA has destroyed something it never
# created.
function Invoke-Uninstall {
    param([string]$InstallDir, [string]$BinaryPath, [string]$ProfilePath)

    $changed = $false

    # 1. the completion block, while there is still a binary to run.
    if ((Test-Path -LiteralPath $BinaryPath) -and $ProfilePath.Length -gt 0) {
        if (Invoke-CompletionUninstall -Exe $BinaryPath -ProfilePath $ProfilePath) { $changed = $true }
    }

    # 2. the binary.
    if (Test-Path -LiteralPath $BinaryPath) {
        try {
            Remove-Item -LiteralPath $BinaryPath -Force -ErrorAction Stop
        } catch {
            if (Test-LockHResult $_.Exception) {
                Show-LockedInstruction $BinaryPath
            } else {
                Write-Err "tasqx installer: could not remove $BinaryPath."
                Write-Err "  $($_.Exception.Message)"
            }
            exit 1
        }
        Write-Output "removed $BinaryPath"
        $changed = $true
    }
    # An install interrupted between the copy and the rename leaves one of
    # these behind, and a directory that still holds one is not empty, so the
    # step below would refuse to clean up over a file this installer wrote.
    Invoke-StaleBinarySweep -Directory $InstallDir

    # 3. the PATH entry.
    $pathState = Revoke-UserPathEntry -Entry $InstallDir
    if ($pathState -eq 'removed') {
        Write-Output "removed $InstallDir from your user PATH."
        $changed = $true
    }

    # 4. the directory, and only the one this script makes for itself. A
    # TASQX_INSTALL the user chose is a directory this installer was pointed
    # at, not one it created, and removing it is not this script's business
    # even when it happens to be empty.
    $default = Get-DefaultInstallDirectory
    $isOurs = $default.Length -gt 0 -and (Get-NormalizedPath $InstallDir) -eq (Get-NormalizedPath $default)
    if ($isOurs -and (Test-Path -LiteralPath $InstallDir)) {
        $left = @(Get-ChildItem -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue)
        if ($left.Count -eq 0) {
            Remove-Item -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue
            $parent = Split-Path -Parent $InstallDir
            $leftParent = @(Get-ChildItem -LiteralPath $parent -Force -ErrorAction SilentlyContinue)
            if ($leftParent.Count -eq 0) {
                Remove-Item -LiteralPath $parent -Force -ErrorAction SilentlyContinue
            }
        }
    }

    if (-not $changed) {
        # Exit 0, not an error. Uninstalling twice, or uninstalling something
        # that was never installed, is an ordinary thing to do and reporting it
        # as a failure sends the user looking for a problem that is not there.
        Write-Output "nothing to remove at $BinaryPath"
    }
    exit 0
}

# -DryRun -Uninstall: name the three things an uninstall would take out, and
# take none of them out.
#
# The ordering this belongs to was a defect rather than a documentation gap:
# Invoke-Main read -Uninstall before it read -DryRun, so the switch whose entire
# promise is "changes nothing" removed the binary, edited $PROFILE and rewrote
# the user PATH. A dry run that uninstalls is worse than no dry run, because the
# person who typed it chose it in order to be safe.
#
# The binary is deliberately never RUN here, and the registry is only read:
# Get-RawUserPath opens HKCU\Environment read-only, which is what makes naming
# the PATH entry honest instead of guessed.
#
# Every line names a real path rather than a category, because "the PATH entry"
# is exactly the item a reader cannot verify from memory.
function Show-UninstallDryRun {
    param([string]$InstallDir, [string]$BinaryPath, [string]$ProfilePath)

    # The em dash from its code point, for the reason the file header gives: no
    # byte in this script may depend on how the file was decoded.
    $emDash = [char]0x2014
    Write-Output "tasqx installer (dry run $emDash nothing will be removed)"

    if (Test-Path -LiteralPath $BinaryPath) {
        Write-Output ('  ' + 'binary'.PadRight(13) + $BinaryPath)
    } else {
        Write-Output ('  ' + 'binary'.PadRight(13) + "$BinaryPath (not there, nothing to remove)")
    }

    if ($ProfilePath.Length -gt 0) {
        Write-Output ('  ' + 'completions'.PadRight(13) + "the tasqx block in $ProfilePath, if it has one")
    } else {
        Write-Output ('  ' + 'completions'.PadRight(13) + 'this host reports no $PROFILE path, so no file would be edited')
    }

    $current = Get-RawUserPath
    if ($null -eq $current -or $current.Length -eq 0) {
        Write-Output ('  ' + 'PATH entry'.PadRight(13) + 'your user PATH read back empty, so it would be left untouched')
    } elseif ((Find-PathEntry -Current $current -Entry $InstallDir) -ge 0) {
        Write-Output ('  ' + 'PATH entry'.PadRight(13) + "$InstallDir would be cut out of your user PATH")
    } else {
        Write-Output ('  ' + 'PATH entry'.PadRight(13) + "$InstallDir is not in your user PATH")
    }

    $default = Get-DefaultInstallDirectory
    $isOurs = $default.Length -gt 0 -and (Get-NormalizedPath $InstallDir) -eq (Get-NormalizedPath $default)
    if ($isOurs) {
        Write-Output ('  ' + 'directory'.PadRight(13) + "$InstallDir, and only if removing the binary leaves it empty")
    } else {
        Write-Output ('  ' + 'directory'.PadRight(13) + "$InstallDir is yours (TASQX_INSTALL) and is left alone")
    }

    Write-Output ('  ' + 'store'.PadRight(13) + 'never touched, here or in a real uninstall')
}

function Invoke-Main {
    param(
        [switch]$DryRun,
        [switch]$Uninstall,
        [switch]$Completions,
        [switch]$Help
    )

    if ($Help -or (Test-EnvSwitch 'TASQX_HELP')) {
        Show-Help
        exit 0
    }

    # Read BEFORE the two action branches below, and that placement is the whole
    # of the fix: while it was computed further down, -Uninstall and
    # -Completions had both already done their work by the time anything asked
    # whether this was a dry run.
    $dry = $DryRun -or (Test-EnvSwitch 'TASQX_DRY_RUN')

    # $PROFILE is expanded HERE, by the PowerShell that is running this script,
    # because that is the only process on the machine that knows its value.
    $profilePath = ''
    if ($null -ne $PROFILE) { $profilePath = ([string]$PROFILE).Trim() }

    # Both of these need the destination, and neither needs the platform
    # mapping or a release tag: uninstalling on a machine tasqx publishes no
    # build for still has to work.
    if ($Uninstall -or (Test-EnvSwitch 'TASQX_UNINSTALL')) {
        $dir = Resolve-InstallDirectory
        if ($dir.Length -eq 0) {
            Write-Err 'tasqx installer: LOCALAPPDATA is unset, so there is no default destination.'
            Write-Err '  Set TASQX_INSTALL to the directory tasqx.exe was installed into.'
            exit 2
        }
        if ($dry) {
            Show-UninstallDryRun -InstallDir $dir -BinaryPath (Join-Path $dir 'tasqx.exe') -ProfilePath $profilePath
            exit 0
        }
        Invoke-Uninstall -InstallDir $dir -BinaryPath (Join-Path $dir 'tasqx.exe') -ProfilePath $profilePath
    }

    # Completions only, per -Help. The bare one-liner never edits a profile:
    # D57 built this feature to ask before writing, and an installer that adds
    # a block to $PROFILE unasked breaks that promise.
    if ($Completions -or (Test-EnvSwitch 'TASQX_COMPLETIONS')) {
        $dir = Resolve-InstallDirectory
        $exe = ''
        if ($dir.Length -gt 0 -and (Test-Path -LiteralPath (Join-Path $dir 'tasqx.exe'))) {
            $exe = Join-Path $dir 'tasqx.exe'
        } else {
            $found = @(Get-Command tasqx -CommandType Application -ErrorAction SilentlyContinue) | Select-Object -First 1
            if ($null -ne $found) { $exe = [string]$found.Source }
        }
        # Every failure below is a warning and exit 0. Nothing here can leave
        # an install broken, because nothing here installs anything.
        if ($exe.Length -eq 0) {
            Write-Err 'warning: no tasqx binary to ask for the completion line.'
            Write-Err '  Install tasqx first, then run this again with -Completions.'
            exit 0
        }
        if ($profilePath.Length -eq 0) {
            Write-Err 'warning: this PowerShell host reports no $PROFILE path, so there is no file to edit.'
            Write-Err "  Name one yourself: $exe completions powershell --install --profile <PATH>"
            exit 0
        }
        # Last, after the two warnings above, so a dry run reports the same
        # refusals a real run would rather than a plan that could not happen.
        if ($dry) {
            $emDash = [char]0x2014
            Write-Output "tasqx installer (dry run $emDash nothing will be written)"
            Write-Output ('  ' + 'completions'.PadRight(13) + "would run: $exe completions powershell --install --profile $profilePath -y")
            Write-Output ('  ' + 'profile'.PadRight(13) + "$profilePath is not edited by this run")
            exit 0
        }
        Invoke-CompletionInstall -Exe $exe -ProfilePath $profilePath | Out-Null
        exit 0
    }

    $arch = Get-HostArchitecture
    $target = ''
    $emulationNote = ''
    switch ($arch.ToUpperInvariant()) {
        'AMD64' {
            $target = 'x86_64-pc-windows-msvc'
        }
        'ARM64' {
            # There is no aarch64-pc-windows-msvc in the release matrix
            # (.github/workflows/release.yml) and this is not the place to add
            # one. Windows runs the x64 build under emulation; say so, rather
            # than let it look like a native install.
            $target = 'x86_64-pc-windows-msvc'
            $emulationNote = 'note: tasqx publishes no native ARM64 build, so the x86_64 build is used and Windows runs it under emulation.'
        }
        default {
            Show-UnmappedPlatform $arch
            exit 2
        }
    }

    $installDir = Resolve-InstallDirectory
    if ($installDir.Length -eq 0) {
        Write-Err 'tasqx installer: LOCALAPPDATA is unset, so there is no default destination.'
        Write-Err '  Set TASQX_INSTALL to the directory tasqx.exe should be installed into.'
        exit 2
    }
    $binaryPath = Join-Path $installDir 'tasqx.exe'

    # TASQX_VERSION is normalised to the v form once, here, so the tag that
    # reaches the URL and the tag that reaches the filename are one string.
    $requested = $env:TASQX_VERSION
    $tag = $null
    $versionSource = ''
    if ($null -ne $requested -and $requested.Trim().Length -gt 0) {
        $requested = $requested.Trim()
        $candidate = $requested
        if ($candidate.StartsWith('v', [StringComparison]::Ordinal)) {
            $candidate = $candidate.Substring(1)
        }
        $candidate = "v$candidate"
        if (-not (Test-ReleaseTag $candidate)) {
            Write-Err "tasqx installer: TASQX_VERSION='$requested' is not a release tag of the form v1.2.3."
            exit 2
        }
        $tag = $candidate
        $versionSource = '(from $TASQX_VERSION)'
    } else {
        $tag = Resolve-ReleaseTag
        if ($null -eq $tag) { exit 2 }
        $versionSource = '(resolved from releases/latest)'
    }

    # The archive keeps the tag's leading v: release.yml stamps the unstripped
    # ref into the filename, so tasqx-0.3.0-... 404s on every download.
    $archiveUrl = "$TasqxRepoUrl/releases/download/$tag/tasqx-$tag-$target.zip"

    if ($emulationNote.Length -gt 0) { Write-Output $emulationNote }

    if ($dry) {
        # These four field labels are a contract, not a debugging aid: install.sh
        # prints the same ones and CI asserts on them.
        # The em dash is built from its code point rather than typed, so this
        # file stays pure ASCII and the header cannot depend on how the file was
        # decoded. A UTF-8 BOM would fix the decoding too, and was tried: on
        # Windows PowerShell 5.1 `irm` hands the BOM straight through, the
        # U+FEFF becomes the first character of the string [scriptblock]::Create
        # parses, the param block below is never seen, and the documented
        # one-liner silently runs an install instead of a dry run.
        $emDash = [char]0x2014
        Write-Output "tasqx installer (dry run $emDash nothing will be written)"
        Write-Output ('  version    ' + $tag.PadRight(18) + $versionSource)
        Write-Output "  platform   Windows $arch -> $target"
        Write-Output "  archive    $archiveUrl"
        Write-Output "  install to $binaryPath"
        exit 0
    }

    # Expand-Archive draws a progress bar, and on Windows PowerShell 5.1 drawing
    # it costs roughly an order of magnitude more than the unpacking it reports
    # on. Set once here rather than beside the call, so anything added below
    # inherits it.
    $ProgressPreference = 'SilentlyContinue'

    # Named with a fixed prefix so a leftover is recognisable as this
    # installer's. It should never survive the finally below, including on a
    # failure, which is what the block exists to guarantee.
    $temp = Join-Path ([IO.Path]::GetTempPath()) ('tasqx-install-' + [Guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Path $temp -Force | Out-Null
    try {
        $archiveName = "tasqx-$tag-$target.zip"
        $archivePath = Join-Path $temp $archiveName
        $sumPath = "$archivePath.sha256"

        Write-Output "downloading $archiveName"
        if (-not (Save-RemoteFile -Uri $archiveUrl -OutFile $archivePath -What 'archive')) { exit 1 }
        if (-not (Save-RemoteFile -Uri "$archiveUrl.sha256" -OutFile $sumPath -What 'checksum')) { exit 1 }

        # Verified BEFORE anything is unpacked. Expand-Archive on an archive
        # nobody has checked is the whole failure this step exists to prevent,
        # so there is no ordering in which it comes second.
        $expected = Get-PublishedSha256 $sumPath
        if ($expected -notmatch '^[0-9a-fA-F]{64}$') {
            Write-Err 'tasqx installer: the published checksum file did not contain a SHA-256.'
            Write-Err "  $archiveUrl.sha256"
            Write-Err "  read: '$expected'"
            exit 1
        }
        $actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash

        # -ne on two strings is case-insensitive in PowerShell, and the two
        # sides genuinely differ in case: Get-FileHash returns upper, release.yml
        # writes lower. Both sums are named, because a mismatch message that
        # shows one of them tells the reader nothing they can act on.
        if ($actual -ne $expected) {
            Write-Err 'tasqx installer: the downloaded archive does not match its published checksum.'
            Write-Err "  archive   $archiveUrl"
            Write-Err "  expected  $expected"
            Write-Err "  actual    $actual"
            Write-Err '  Nothing was unpacked and nothing was installed.'
            exit 1
        }
        Write-Output "sha256 verified $($expected.ToLowerInvariant())"

        # -LiteralPath, never -Path. -Path treats its argument as a wildcard
        # pattern, so a TEMP directory containing a `[` fails in a way that
        # reads as a corrupt archive.
        $unpacked = Join-Path $temp 'unpacked'
        try {
            Expand-Archive -LiteralPath $archivePath -DestinationPath $unpacked -Force -ErrorAction Stop
        } catch {
            Write-Err 'tasqx installer: could not unpack the archive.'
            Write-Err "  $($_.Exception.Message)"
            exit 1
        }

        # Located, not computed. Compress-Archive was handed the staging
        # directory itself (release.yml:171), so everything sits one level down
        # under tasqx-<tag>-<target>\ -- observed behaviour of the packaging
        # step that nothing in CI asserts. A computed path is a second copy of
        # that assumption, and it fails as "file not found" the day the
        # packaging step changes.
        $candidates = @(Get-ChildItem -LiteralPath $unpacked -Recurse -Filter 'tasqx.exe' -File -ErrorAction SilentlyContinue)
        if ($candidates.Count -eq 0) {
            Write-Err 'tasqx installer: the archive contained no tasqx.exe.'
            Write-Err "  $archiveUrl"
            exit 1
        }
        if ($candidates.Count -gt 1) {
            Write-Err "tasqx installer: the archive contained $($candidates.Count) files named tasqx.exe. Refusing to guess."
            foreach ($candidate in $candidates) {
                Write-Err "  $($candidate.FullName.Substring($unpacked.Length).TrimStart('\'))"
            }
            exit 1
        }

        if (-not (Install-Binary -Source $candidates[0].FullName -Destination $binaryPath)) { exit 1 }

        # The PATH is written before the report and announced after it, so the
        # lines read in the order a user would tell the story: what was
        # installed, then what was done to make it findable. Show-InstallReport
        # asks the shell what `tasqx` resolves to, which is why the session's
        # own $env:Path is updated first -- otherwise it answers about the
        # process that started before the entry existed.
        $pathState = Add-UserPathEntry -Entry $installDir
        Add-SessionPathEntry -Entry $installDir

        Show-InstallReport -Tag $tag -InstallDir $installDir -BinaryPath $binaryPath

        if ($pathState -eq 'added') {
            Write-Output 'Added to your user PATH. Open a new terminal for it to take effect.'
        } elseif ($pathState -eq 'present') {
            Write-Output "$installDir is already on your PATH."
        }
        exit 0
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# An unrecognised argument does not stop a plain param() block. PowerShell drops
# it into $args and runs the script anyway, so `-DryRunn` and `-Dry-Run` both
# bind nothing, leave $DryRun false, and fall through to whatever the default
# path is. That was harmless while the default path was a stub. It is not
# harmless now: the default path installs, so a typo installs.
#
# $args is read here, at script scope, because inside a function $args means
# that function's own arguments. Same shape and same exit code as install.sh:
# the offending word, then the usage text, both on stderr, exit 2.
if ($args.Count -gt 0) {
    Write-Err "unknown option: $($args[0])"
    Show-Help | ForEach-Object { Write-Err $_ }
    exit 2
}

Invoke-Main -DryRun:$DryRun -Uninstall:$Uninstall -Completions:$Completions -Help:$Help
