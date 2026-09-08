# ReAI Vibe Board Virtual Microphone — Windows driver

SysVAD-derived PortCls/WaveRT **capture-only** virtual microphone, based on
Microsoft's MIT-licensed
[SimpleAudioSample](https://github.com/microsoft/Windows-driver-samples/tree/main/audio/simpleaudiosample)
(the render/speaker endpoint pair was removed, the mic endpoint was narrowed to
one native format, and the tone generator was replaced by a user-mode PCM
feed). It exposes the BLE-connected ReAI Vibe Board as a system-level input
device for the SDK's `virtual-mic` feature — the Windows counterpart of the
macOS CoreAudio HAL plugin (`virtual-mic/driver/`). License: MIT (matching the
SDK); the Microsoft sample it derives from is MIT too.

## What the driver provides

| Piece | Value |
|---|---|
| Capture endpoint | **"ReAI-Vibe-Board"**, 16 kHz mono 16-bit (native; the Windows audio engine resamples for apps) |
| Root device | `ROOT\ReAIVB` (devnode `ROOT\REAIVB\0000`), service `ReAIVibeBoardVirtualMic` |
| Control device | `\\.\ReAIVibeBoardVirtualMic` — `IRP_MJ_WRITE` accepts little-endian S16 PCM, 16 kHz mono |
| Feed policy | bounded 128 KB ring (~4 s): underflow renders **silence**, overflow **drops** the incoming remainder — latency never accumulates |

Derivation highlights (vs. the sample):

- speaker/HDMI endpoints removed (`g_RenderEndpoints` is empty);
- `micarraywavtable.h` narrowed to a single 16 kHz / mono / 16-bit range;
- mic-array geometry reports a single centered microphone;
- `ToneGenerator` replaced by `Source/Main/pcmfeed.*` (lock-protected ring);
- `Source/Main/vbmcontrol.*` adds the control device: created in `AddDevice`,
  deleted on device removal / unload, dispatch wrappers in `DriverEntry` route
  `IRP_MJ_CREATE/CLEANUP/WRITE` for the control device and forward everything
  else to `PcDispatchIrp` unchanged;
- the DRM `SignatureAttributes` section was dropped (no protected content).

## Build (WDK required)

Visual Studio 2022 with the C++ workload **and the Windows Driver Kit**, then:

```powershell
# from the repository root
pwsh -File scripts/build-driver-windows.ps1 -Platform x64 -Configuration Release -TestSign
# → virtual-mic/driver-windows/out/Release-x64/{ReAIVibeBoardVirtualMic.sys,.inf,.cat,testsign.cer}
```

On **Build Tools-only** machines the WDK installer may skip VS integration
(`error MSB8020: WindowsKernelModeDriver10.0 build tools not found`). Fix by
adding the individual components in the Visual Studio Installer:

- `Component.Microsoft.Windows.DriverKit.BuildTools` (the WDK VSIX)
- `Microsoft.VisualStudio.Component.VC.14.44.17.14.x86.x64.Spectre`
  (MSVC Spectre-mitigated libs — the driver toolset requires them)

The build passes `/p:SignMode=Off` so the WDK's built-in SignTask (which wants
an elevated certificate container) is skipped; `-TestSign` does the signing
with signtool against a CurrentUser self-signed cert instead.

Or grab the test-signed artifact from the repo's **driver-windows** GitHub
Actions workflow (runs on `windows-2022` runners where the WDK is still
preinstalled; `windows-2025` images dropped it — actions/runner-images#13071).
Repeat with `-Platform ARM64` for ARM64 machines.

## Install (test-signed, development only)

1. Enable test signing and reboot (suspends BitLocker/Secure Boot — re-enable
   after testing):

   ```bat
   bcdedit /set TESTSIGNING ON
   ```

2. Import `testsign.cer` into **Trusted Root Certification Authorities** and
   **Trusted Publishers** (Local Machine).
3. Install — either elevated pnputil directly:

   ```bat
   pnputil /add-driver ReAIVibeBoardVirtualMic.inf /install
   ```

   or just let the SDK do it: set `REAI_VIRTUAL_MIC_DRIVER_DIR` to the package
   directory and call `VirtualMic::ensure_installed()` (shows a UAC prompt).
4. Verify: **Settings → System → Sound → Input** should list
   **"ReAI-Vibe-Board"**. Feed it PCM (`cargo run --features virtual-mic
   --example virtual_mic_demo` over BLE) or test standalone with a WAV writer
   opening `\\.\ReAIVibeBoardVirtualMic` and writing S16 bytes.

Remove the device with `VirtualMic::uninstall()` (`pnputil /remove-device
ROOT\REAIVB\0000`); the package stays in the Driver Store, so re-installing is
cheap. Fully purge with `pnputil /delete-driver oemXX.inf /force`.

## Production signing (not done here)

Real user distribution needs EV code-signing for the binaries plus
[Microsoft Partner Center attestation signing](https://learn.microsoft.com/windows-hardware/drivers/dashboard/attestation-signing-a-kernel-driver-for-public-release)
for the `.cat`, so the driver loads without test-signing mode on end-user
machines. That is issue #11's separate phase-two; this driver intentionally
does not bundle certificates or signing logic.
