[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Directory,
    [ValidateSet("Data", "Application")]
    [string]$Purpose = "Data",
    [switch]$ValidateControlFiles,
    [switch]$Harden,
    [switch]$ValidateAcl
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$systemSid = [Security.Principal.SecurityIdentifier]::new("S-1-5-18")
$administratorsSid = [Security.Principal.SecurityIdentifier]::new("S-1-5-32-544")
$usersSid = [Security.Principal.SecurityIdentifier]::new("S-1-5-32-545")
$allowedOwnerSids = @($systemSid.Value, $administratorsSid.Value)
$writeRights = [Security.AccessControl.FileSystemRights]::WriteData -bor
    [Security.AccessControl.FileSystemRights]::AppendData -bor
    [Security.AccessControl.FileSystemRights]::WriteExtendedAttributes -bor
    [Security.AccessControl.FileSystemRights]::WriteAttributes -bor
    [Security.AccessControl.FileSystemRights]::Delete -bor
    [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
    [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
    [Security.AccessControl.FileSystemRights]::TakeOwnership
$inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
    [Security.AccessControl.InheritanceFlags]::ObjectInherit
$noInheritance = [Security.AccessControl.InheritanceFlags]::None
$noPropagation = [Security.AccessControl.PropagationFlags]::None
$allow = [Security.AccessControl.AccessControlType]::Allow
$reparsePoint = [IO.FileAttributes]::ReparsePoint
$systemFsutil = Join-Path ([Environment]::SystemDirectory) "fsutil.exe"

if ($null -eq ("WinSchedDirectoryHandle" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

public static class WinSchedDirectoryHandle
{
    private const uint FileReadAttributes = 0x00000080;
    private const uint ReadControl = 0x00020000;
    private const uint FileShareRead = 0x00000001;
    private const uint OpenExisting = 3;
    private const uint FileFlagBackupSemantics = 0x02000000;
    private const uint FileFlagOpenReparsePoint = 0x00200000;
    private const uint FileAttributeReparsePoint = 0x00000400;
    private const int FileAttributeTagInfo = 9;

    [StructLayout(LayoutKind.Sequential)]
    private struct FileAttributeTagInformation
    {
        internal uint FileAttributes;
        internal uint ReparseTag;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFileInformationByHandleEx(
        SafeFileHandle fileHandle,
        int fileInformationClass,
        out FileAttributeTagInformation fileInformation,
        uint bufferSize);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandleW(
        SafeFileHandle fileHandle,
        StringBuilder filePath,
        uint filePathLength,
        uint flags);

    public static SafeFileHandle OpenNoFollow(string path)
    {
        SafeFileHandle handle = CreateFileW(
            path,
            FileReadAttributes | ReadControl,
            FileShareRead,
            IntPtr.Zero,
            OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero);
        if (handle.IsInvalid)
        {
            int error = Marshal.GetLastWin32Error();
            handle.Dispose();
            throw new Win32Exception(error, "Cannot lock the WinSched directory without following reparse points.");
        }
        return handle;
    }

    public static bool IsReparsePoint(SafeFileHandle handle)
    {
        FileAttributeTagInformation information;
        if (!GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            out information,
            (uint)Marshal.SizeOf(typeof(FileAttributeTagInformation))))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        return (information.FileAttributes & FileAttributeReparsePoint) != 0;
    }

    public static string GetFinalPath(SafeFileHandle handle)
    {
        StringBuilder path = new StringBuilder(1024);
        uint length = GetFinalPathNameByHandleW(handle, path, (uint)path.Capacity, 0);
        if (length == 0 || length >= path.Capacity)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        string result = path.ToString();
        if (result.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase))
        {
            result = @"\\" + result.Substring(8);
        }
        else if (result.StartsWith(@"\\?\", StringComparison.OrdinalIgnoreCase))
        {
            result = result.Substring(4);
        }
        return Path.GetFullPath(result).TrimEnd('\\');
    }
}
'@
}

function Get-NormalizedPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    $root = [IO.Path]::GetPathRoot($full).TrimEnd('\')
    if ([string]::IsNullOrWhiteSpace($full) -or
        [string]::Equals($full, $root, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing a root or empty WinSched data path: $Path"
    }
    $expectedBase = if ($Purpose -eq "Data") {
        [Environment]::GetFolderPath("CommonApplicationData")
    } else {
        [Environment]::GetFolderPath("ProgramFiles")
    }
    $expected = [IO.Path]::GetFullPath((Join-Path $expectedBase "WinSched")).TrimEnd('\')
    if (-not [string]::Equals($full, $expected, [StringComparison]::OrdinalIgnoreCase)) {
        throw "WinSched $Purpose ACL helper accepts only the fixed path: $expected"
    }
    return $full
}

function Assert-NotReparsePoint([string]$Path) {
    $attributes = [IO.File]::GetAttributes($Path)
    if (($attributes -band $reparsePoint) -ne 0) {
        throw "Refusing a reparse point in the WinSched data tree: $Path"
    }
}

function Get-TreeEntries([string]$Root) {
    $entries = New-Object Collections.Generic.List[string]
    Assert-NotReparsePoint $Root
    foreach ($entry in [IO.Directory]::EnumerateFileSystemEntries($Root)) {
        Assert-NotReparsePoint $entry
        if ([IO.Directory]::Exists($entry)) {
            throw "Subdirectories are not allowed in the flat WinSched $Purpose directory: $entry"
        }
        $entries.Add($entry)
        if ($entries.Count -gt 1000) {
            throw "The WinSched $Purpose directory exceeds the 1000-file hardening limit."
        }
    }
    return $entries.ToArray()
}

function Test-AllowedFileName([string]$Name) {
    if ($Purpose -eq "Application") {
        return $Name -match '^(winsched(-service|-monitor|-tray|-settings)?\.exe|README\.md|LICENSE|unins[0-9]+\.(exe|dat|msg)|\.winsched-atomic-[0-9]+-[0-9]+\.tmp)$'
    }
    return $Name -match '^(winsched(-service|-monitor|-tray|-settings)?\.exe|winsched\.toml|winsched-settings\.lock|winsched(-emergency)?\.log(\.(10|[1-9]))?|status\.json|provision-result\.txt|setup-provenance\.txt|(managed-state|background-state|runtime-state)\.(json|bak)|install\.ps1|uninstall\.ps1|secure-data\.ps1|Install WinSched\.cmd|Uninstall WinSched\.cmd|README\.md|LICENSE|SHA256SUMS|\.winsched-atomic-[0-9]+-[0-9]+\.tmp)$'
}

function Test-StaleAtomicFile([string]$Path) {
    return [IO.Path]::GetFileName($Path) -match '^\.winsched-atomic-[0-9]+-[0-9]+\.tmp$'
}

function Assert-SingleHardLink([string]$Path) {
    $links = @(& $systemFsutil hardlink list $Path 2>$null | Where-Object {
        -not [string]::IsNullOrWhiteSpace([string]$_)
    })
    if (($LASTEXITCODE -ne 0) -or ($links.Count -ne 1)) {
        throw "Refusing a multiply linked or unqueryable WinSched file: $Path"
    }
}

function Get-OwnerSid([Security.AccessControl.FileSystemSecurity]$Acl) {
    return $Acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
}

function Assert-TrustedControlFile([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "A WinSched control-file path is not a regular file: $Path"
    }
    Assert-NotReparsePoint $Path
    if (-not (Test-AllowedFileName ([IO.Path]::GetFileName($Path)))) {
        throw "Unexpected file in WinSched $Purpose directory: $Path"
    }
    Assert-SingleHardLink $Path
    $acl = Get-Acl -LiteralPath $Path
    $owner = Get-OwnerSid $acl
    if ($allowedOwnerSids -notcontains $owner) {
        throw "Untrusted owner on WinSched control file $Path ($owner)."
    }
    foreach ($rule in $acl.GetAccessRules(
        $true,
        $true,
        [Security.Principal.SecurityIdentifier]
    )) {
        if ($rule.AccessControlType -ne $allow) {
            continue
        }
        $sid = $rule.IdentityReference.Value
        if (($allowedOwnerSids -notcontains $sid) -and
            (($rule.FileSystemRights -band $writeRights) -ne 0)) {
            throw "Untrusted writable ACL on WinSched control file $Path ($sid)."
        }
    }
}

function New-SecureAcl([bool]$IsDirectory) {
    $acl = if ($IsDirectory) {
        New-Object Security.AccessControl.DirectorySecurity
    } else {
        New-Object Security.AccessControl.FileSecurity
    }
    $acl.SetAccessRuleProtection($true, $false)
    $flags = if ($IsDirectory) { $inheritance } else { $noInheritance }
    $acl.AddAccessRule(([Security.AccessControl.FileSystemAccessRule]::new(
        $systemSid,
        [Security.AccessControl.FileSystemRights]::FullControl,
        $flags,
        $noPropagation,
        $allow
    )))
    $acl.AddAccessRule(([Security.AccessControl.FileSystemAccessRule]::new(
        $administratorsSid,
        [Security.AccessControl.FileSystemRights]::FullControl,
        $flags,
        $noPropagation,
        $allow
    )))
    $acl.AddAccessRule(([Security.AccessControl.FileSystemAccessRule]::new(
        $usersSid,
        [Security.AccessControl.FileSystemRights]::ReadAndExecute,
        $flags,
        $noPropagation,
        $allow
    )))
    $acl.SetOwner($administratorsSid)
    return $acl
}

function Set-SecureAcl([string]$Path) {
    $isDirectory = [IO.Directory]::Exists($Path)
    $acl = New-SecureAcl $isDirectory
    if ($isDirectory) {
        [IO.Directory]::SetAccessControl($Path, $acl)
    } else {
        [IO.File]::SetAccessControl($Path, $acl)
    }
}

function Assert-SecureAcl([string]$Path) {
    Assert-NotReparsePoint $Path
    $isDirectory = [IO.Directory]::Exists($Path)
    $acl = Get-Acl -LiteralPath $Path
    if ($isDirectory -and -not $acl.AreAccessRulesProtected) {
        throw "WinSched ACL inheritance is still enabled: $Path"
    }
    if ((Get-OwnerSid $acl) -ne $administratorsSid.Value) {
        throw "WinSched path is not owned by Administrators: $Path"
    }
    $seenSystem = $false
    $seenAdministrators = $false
    $seenUsers = $false
    $ruleCount = 0
    $expectedFlags = if ($isDirectory) { $inheritance } else { $noInheritance }
    $usersReadRights = [Security.AccessControl.FileSystemRights]::ReadAndExecute -bor
        [Security.AccessControl.FileSystemRights]::Synchronize
    foreach ($rule in $acl.GetAccessRules(
        $true,
        $true,
        [Security.Principal.SecurityIdentifier]
    )) {
        $ruleCount++
        if ($rule.AccessControlType -ne $allow) {
            throw "Unexpected deny ACL on WinSched path: $Path"
        }
        if ($rule.InheritanceFlags -ne $expectedFlags -or
            $rule.PropagationFlags -ne $noPropagation) {
            throw "Unexpected inheritance flags on WinSched path: $Path"
        }
        if (($isDirectory -and $rule.IsInherited) -or
            ((-not $isDirectory) -and
                ($rule.IsInherited -eq $acl.AreAccessRulesProtected))) {
            throw "Unexpected explicit/inherited ACL form on WinSched path: $Path"
        }
        $sid = $rule.IdentityReference.Value
        if ($sid -eq $systemSid.Value) {
            $seenSystem =
                $rule.FileSystemRights -eq [Security.AccessControl.FileSystemRights]::FullControl
        } elseif ($sid -eq $administratorsSid.Value) {
            $seenAdministrators =
                $rule.FileSystemRights -eq [Security.AccessControl.FileSystemRights]::FullControl
        } elseif ($sid -eq $usersSid.Value) {
            if (($rule.FileSystemRights -band $writeRights) -ne 0) {
                throw "Users retain write access to WinSched path: $Path"
            }
            $seenUsers = $rule.FileSystemRights -eq $usersReadRights
        } else {
            throw "Unexpected identity in WinSched ACL: $Path ($sid)"
        }
    }
    if ($ruleCount -ne 3 -or
        -not ($seenSystem -and $seenAdministrators -and $seenUsers)) {
        throw "Required WinSched ACL entries are missing: $Path"
    }
}

$Directory = Get-NormalizedPath $Directory
$createdDirectory = $false
if (-not (Test-Path -LiteralPath $Directory)) {
    if (-not $Harden) {
        throw "WinSched data directory does not exist: $Directory"
    }
    New-Item -ItemType Directory -Path $Directory | Out-Null
    $createdDirectory = $true
}
if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
    throw "WinSched data path is not a directory: $Directory"
}

$rootHandle = [WinSchedDirectoryHandle]::OpenNoFollow($Directory)
try {
    if ([WinSchedDirectoryHandle]::IsReparsePoint($rootHandle)) {
        throw "Refusing a reparse point for the WinSched $Purpose directory: $Directory"
    }
    $lockedPath = [WinSchedDirectoryHandle]::GetFinalPath($rootHandle)
    if (-not [string]::Equals(
        $lockedPath,
        $Directory,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "The locked WinSched $Purpose directory resolved to an unexpected path: $lockedPath"
    }
    $rootAcl = Get-Acl -LiteralPath $Directory
    $rootOwner = Get-OwnerSid $rootAcl
    if ((-not $createdDirectory) -and ($allowedOwnerSids -notcontains $rootOwner)) {
        throw "Refusing an untrusted owner on the WinSched $Purpose directory ($rootOwner)."
    }
    if ($Harden) {
        # Denying delete sharing pins this exact non-reparse directory while its
        # ACL is replaced. Child inventory starts only after unprivileged writes
        # have been removed from the root.
        Set-SecureAcl $Directory
        if ([WinSchedDirectoryHandle]::IsReparsePoint($rootHandle)) {
            throw "The locked WinSched $Purpose directory became a reparse point."
        }
        if ((Get-OwnerSid (Get-Acl -LiteralPath $Directory)) -ne $administratorsSid.Value) {
            throw "WinSched $Purpose directory owner did not stabilize after hardening."
        }
    }
} finally {
    $rootHandle.Dispose()
}
$entries = @(Get-TreeEntries $Directory)
foreach ($entry in $entries) {
    Assert-TrustedControlFile $entry
}
$expectedEntries = New-Object Collections.Generic.List[string]
if ($Harden) {
    foreach ($entry in $entries) {
        if (Test-StaleAtomicFile $entry) {
            Remove-Item -LiteralPath $entry -Force
        } else {
            Set-SecureAcl $entry
            $expectedEntries.Add($entry)
        }
    }
} else {
    foreach ($entry in $entries) {
        $expectedEntries.Add($entry)
    }
}
$finalEntries = @(Get-TreeEntries $Directory)
$initialSorted = @($expectedEntries.ToArray() | Sort-Object)
$finalSorted = @($finalEntries | Sort-Object)
$treeChanged = $initialSorted.Count -ne $finalSorted.Count
if (-not $treeChanged) {
    for ($index = 0; $index -lt $initialSorted.Count; $index++) {
        if (-not [string]::Equals(
            $initialSorted[$index],
            $finalSorted[$index],
            [StringComparison]::OrdinalIgnoreCase
        )) {
            $treeChanged = $true
            break
        }
    }
}
if ($treeChanged) {
    throw "The WinSched $Purpose directory changed during ACL hardening."
}
if ($ValidateAcl -or $Harden) {
    Assert-SecureAcl $Directory
    foreach ($entry in $finalEntries) {
        Assert-SecureAcl $entry
    }
}

Write-Output "WinSched $Purpose ACL verified: $Directory ($($finalEntries.Count) files)"
