# freeseries setup: installs the Microsoft-signed fxvad virtual audio device and names its endpoint.
# Runs elevated, started by the app. Windows PowerShell 5.1 compatible (that is what every PC has).
#
#   -DryRun      check the package and report what would happen; needs no admin rights
#   -RenameOnly  skip the install, only (re)name the endpoint
#   -AddInstance install one more instance of the device even though one exists (channels experiment)
#
# Rollback: pnputil /remove-device "ROOT\MEDIA\000x", then pnputil /delete-driver <oemNN.inf> /uninstall
param(
    [Parameter(Mandatory = $true)][string]$Pkg,
    [Parameter(Mandatory = $true)][string]$Result,
    # One name per driver instance, handed out in this order to endpoints that don't have one yet.
    [string[]]$Names = @("freeseries", "freeseries Chat", "freeseries Media"),
    [switch]$DryRun,
    [switch]$RenameOnly,
    [switch]$AddInstance
)
$ErrorActionPreference = 'Stop'
$log = New-Object System.Collections.Generic.List[string]
function Say([string]$line) { $log.Add($line); Write-Output $line }
function Finish([string]$status) {
    # First line is what the app reads; the rest is detail for the log.
    Set-Content -Path $Result -Value (@($status) + $log) -Encoding UTF8
    if ($status -eq 'OK') { exit 0 } else { exit 1 }
}

try {
    foreach ($f in 'fxvad.sys', 'fxvadntamd64.cat') {
        $sig = Get-AuthenticodeSignature (Join-Path $Pkg $f)
        if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'Microsoft Windows Hardware Compatibility Publisher') {
            Finish "FAILED: $f is not signed by Microsoft ($($sig.Status)). The download may be damaged."
        }
    }
    Say "driver package signatures verified"

    Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text; using System.Collections.Generic;

// Creates the Root\FXVAD devnode the same way Microsoft's `devcon install` does.
public static class RootDev {
  [StructLayout(LayoutKind.Sequential)] struct SP_DEVINFO_DATA { public uint cbSize; public Guid ClassGuid; public uint DevInst; public IntPtr Reserved; }
  [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiGetINFClass(string inf, out Guid classGuid, StringBuilder className, uint size, out uint required);
  [DllImport("setupapi.dll", SetLastError=true)] static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid classGuid, IntPtr hwnd);
  [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiCreateDeviceInfo(IntPtr set, string name, ref Guid classGuid, string desc, IntPtr hwnd, uint flags, ref SP_DEVINFO_DATA data);
  [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiSetDeviceRegistryProperty(IntPtr set, ref SP_DEVINFO_DATA data, uint prop, byte[] buf, uint size);
  [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiCallClassInstaller(uint fn, IntPtr set, ref SP_DEVINFO_DATA data);
  [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
  [DllImport("newdev.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool UpdateDriverForPlugAndPlayDevices(IntPtr hwnd, string hwid, string inf, uint flags, out bool reboot);
  const uint DICD_GENERATE_ID = 1, SPDRP_HARDWAREID = 1, DIF_REGISTERDEVICE = 0x19, DIF_REMOVE = 5, INSTALLFLAG_FORCE = 1;

  static Exception Fail(string step) { int code = Marshal.GetLastWin32Error(); return new Exception(step + " failed: 0x" + code.ToString("X8") + " " + new System.ComponentModel.Win32Exception(code).Message); }

  public static bool Install(string inf, string hwid) {
    Guid cls; uint req; var name = new StringBuilder(64);
    if (!SetupDiGetINFClass(inf, out cls, name, (uint)name.Capacity, out req)) throw Fail("SetupDiGetINFClass");
    IntPtr set = SetupDiCreateDeviceInfoList(ref cls, IntPtr.Zero);
    if (set == new IntPtr(-1)) throw Fail("SetupDiCreateDeviceInfoList");
    try {
      var data = new SP_DEVINFO_DATA(); data.cbSize = (uint)Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
      if (!SetupDiCreateDeviceInfo(set, name.ToString(), ref cls, null, IntPtr.Zero, DICD_GENERATE_ID, ref data)) throw Fail("SetupDiCreateDeviceInfo");
      byte[] ids = Encoding.Unicode.GetBytes(hwid + "\0\0");
      if (!SetupDiSetDeviceRegistryProperty(set, ref data, SPDRP_HARDWAREID, ids, (uint)ids.Length)) throw Fail("SetupDiSetDeviceRegistryProperty");
      if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data)) throw Fail("DIF_REGISTERDEVICE");
      bool reboot;
      if (!UpdateDriverForPlugAndPlayDevices(IntPtr.Zero, hwid, inf, INSTALLFLAG_FORCE, out reboot)) {
        var e = Fail("UpdateDriverForPlugAndPlayDevices");
        SetupDiCallClassInstaller(DIF_REMOVE, set, ref data); // don't leave a driverless devnode behind
        throw e;
      }
      return reboot;
    } finally { SetupDiDestroyDeviceInfoList(set); }
  }
}

// Reads and writes the name Windows shows for an audio endpoint ("Speakers" in "Speakers (FxSound ...)").
public static class Endpoint {
  [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumerator {}
  [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IMMDeviceEnumerator { [PreserveSig] int EnumAudioEndpoints(int flow, int stateMask, out IMMDeviceCollection devices); }
  [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IMMDeviceCollection { [PreserveSig] int GetCount(out int count); [PreserveSig] int Item(int index, out IMMDevice device); }
  [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IMMDevice { [PreserveSig] int Activate(ref Guid iid, int ctx, IntPtr p, out IntPtr o); [PreserveSig] int OpenPropertyStore(int access, out IPropertyStore store); }
  [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IPropertyStore {
    [PreserveSig] int GetCount(out int count); [PreserveSig] int GetAt(int index, out PropertyKey key);
    [PreserveSig] int GetValue(ref PropertyKey key, out PropVariant value); [PreserveSig] int SetValue(ref PropertyKey key, ref PropVariant value); [PreserveSig] int Commit();
  }
  [StructLayout(LayoutKind.Sequential)] struct PropertyKey { public Guid fmtid; public int pid; }
  [StructLayout(LayoutKind.Sequential)] struct PropVariant { public ushort vt; ushort r1, r2, r3; public IntPtr p; public IntPtr p2; }

  static PropertyKey Key(string fmtid, int pid) { return new PropertyKey { fmtid = new Guid(fmtid), pid = pid }; }
  static readonly PropertyKey DeviceDesc = Key("a45c254e-df1c-4efd-8020-67d146a850e0", 2);      // "Speakers"
  static readonly PropertyKey InterfaceName = Key("026e516e-b814-414b-83cd-856d6fef4822", 2);   // "FxSound Audio Enhancer"

  static string Read(IPropertyStore store, PropertyKey key) {
    PropVariant v; if (store.GetValue(ref key, out v) != 0 || v.vt != 31) return ""; // VT_LPWSTR
    return Marshal.PtrToStringUni(v.p) ?? "";
  }

  /// Gives every active render endpoint of `driver` one of `names`: endpoints that already carry
  /// one keep it, the others get the first name still free. With `apply` off it only reports.
  /// (A bool rather than null names: PowerShell hands C# an empty string for $null, and an empty
  /// name is a valid rename.)
  public static string[] Rename(string driver, string[] names, bool apply) {
    foreach (var n in names) if (String.IsNullOrWhiteSpace(n)) throw new ArgumentException("an endpoint name cannot be empty");
    var done = new List<string>();
    var all = (IMMDeviceEnumerator)new MMDeviceEnumerator();
    IMMDeviceCollection devices; Marshal.ThrowExceptionForHR(all.EnumAudioEndpoints(0, 1, out devices)); // eRender, ACTIVE
    int count; devices.GetCount(out count);
    var ours = new List<IMMDevice>(); var current = new List<string>();
    for (int i = 0; i < count; i++) {
      IMMDevice device; devices.Item(i, out device);
      IPropertyStore read; Marshal.ThrowExceptionForHR(device.OpenPropertyStore(0, out read)); // STGM_READ
      if (Read(read, InterfaceName).IndexOf(driver, StringComparison.OrdinalIgnoreCase) < 0) continue;
      ours.Add(device); current.Add(Read(read, DeviceDesc));
    }
    var free = new List<string>(names);
    foreach (var name in current) free.Remove(name);
    for (int i = 0; i < ours.Count; i++) {
      string old = current[i];
      if (Array.IndexOf(names, old) >= 0) { done.Add("'" + old + "' (already named)"); continue; }
      if (free.Count == 0) { done.Add("'" + old + "' (no name left for it)"); continue; }
      string newName = free[0]; free.RemoveAt(0);
      if (!apply) { done.Add("'" + old + "' would become '" + newName + "'"); continue; }
      IPropertyStore write; Marshal.ThrowExceptionForHR(ours[i].OpenPropertyStore(2, out write)); // STGM_READWRITE
      var value = new PropVariant { vt = 31, p = Marshal.StringToCoTaskMemUni(newName) };
      try { var key = DeviceDesc; Marshal.ThrowExceptionForHR(write.SetValue(ref key, ref value)); Marshal.ThrowExceptionForHR(write.Commit()); }
      finally { Marshal.FreeCoTaskMem(value.p); }
      done.Add("'" + old + "' -> '" + newName + "'");
    }
    return done.ToArray();
  }
}
'@

    $present = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains 'Root\FXVAD' -and $_.Status -eq 'OK' })
    if ($RenameOnly) {
        Say "install skipped (rename only)"
    } elseif ($present.Count -gt 0 -and -not $AddInstance) {
        Say "driver already installed: $($present[0].InstanceId)"
    } elseif ($DryRun) {
        Say "would install $(Join-Path $Pkg 'fxvad.inf') as Root\FXVAD"
    } else {
        try {
            Enable-ComputerRestore -Drive "$env:SystemDrive\" -ErrorAction Stop
            Checkpoint-Computer -Description "before freeseries audio device" -RestorePointType DEVICE_DRIVER_INSTALL -ErrorAction Stop
            Say "restore point created"
        } catch { Say "restore point not created: $($_.Exception.Message)" }
        $reboot = [RootDev]::Install((Join-Path $Pkg 'fxvad.inf'), 'Root\FXVAD')
        Say "driver installed (reboot requested: $reboot)"
        Start-Sleep -Seconds 2 # let the audio service publish the new endpoint before naming it
    }

    # The rename is a nicety: never let it turn a successful install into a failure.
    try {
        $renamed = [Endpoint]::Rename('FxSound', $Names, (-not $DryRun))
        if ($renamed.Count -eq 0) { Say "no endpoint to name yet" } else { $renamed | ForEach-Object { Say "endpoint: $_" } }
    } catch { Say "endpoint not renamed: $($_.Exception.Message)" }

    Finish 'OK'
} catch {
    Finish "FAILED: $($_.Exception.Message)"
}
