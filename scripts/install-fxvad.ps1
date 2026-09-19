# Installs the Microsoft-signed fxvad virtual audio driver as a root-enumerated device.
# Same SetupAPI sequence as Microsoft's `devcon install`. Must run elevated.
# Rollback: pnputil /remove-device <InstanceId>  then  pnputil /delete-driver <oemNN.inf> /uninstall
param([string]$Pkg = "C:\fxvad-pkg")
$ErrorActionPreference = 'Continue'
Start-Transcript -Path "$Pkg\install.log" -Force | Out-Null

"secure boot: $(try { Confirm-SecureBootUEFI } catch { $_.Exception.Message })"
"test signing: $((bcdedit /enum '{current}' | Select-String 'testsigning') ?? 'off')"

foreach ($f in 'fxvad.sys', 'fxvadntamd64.cat') {
    $sig = Get-AuthenticodeSignature "$Pkg\$f"
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'Microsoft Windows Hardware Compatibility Publisher') {
        "ABORT: $f signature is $($sig.Status) / $($sig.SignerCertificate.Subject)"; Stop-Transcript | Out-Null; exit 2
    }
}
"signatures re-verified"

try {
    Enable-ComputerRestore -Drive "$env:SystemDrive\" -ErrorAction Stop
    Checkpoint-Computer -Description "before fxvad virtual audio driver" -RestorePointType DEVICE_DRIVER_INSTALL -ErrorAction Stop
    "restore point created"
} catch { "restore point NOT created: $($_.Exception.Message)" }

Add-Type -TypeDefinition @'
using System; using System.ComponentModel; using System.Runtime.InteropServices; using System.Text;
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

  static Exception Fail(string step) { int code = Marshal.GetLastWin32Error(); return new Exception(step + " failed: 0x" + code.ToString("X8") + " " + new Win32Exception(code).Message); }

  /// Returns true if Windows asks for a reboot.
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
'@

if (Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -match '^ROOT\\.*' -and $_.HardwareID -contains 'Root\FXVAD' }) {
    "ABORT: a Root\FXVAD device already exists"; Stop-Transcript | Out-Null; exit 3
}

try {
    $reboot = [RootDev]::Install("$Pkg\fxvad.inf", 'Root\FXVAD')
    "INSTALLED. reboot requested: $reboot"
} catch { "INSTALL FAILED: $($_.Exception.InnerException.Message ?? $_.Exception.Message)" }

"--- resulting device(s)"
Get-PnpDevice | Where-Object { $_.HardwareID -contains 'Root\FXVAD' -or $_.FriendlyName -match 'FxSound' } | Format-Table Status, Class, FriendlyName, InstanceId -AutoSize | Out-String
"--- driver store entry"
(pnputil /enum-drivers | Out-String) -split "(?m)^\s*$" | Where-Object { $_ -match 'fxvad' }
Stop-Transcript | Out-Null
