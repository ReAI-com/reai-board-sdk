/*++

Copyright (c) ReAI Team. All Rights Reserved.

Module Name:

    vbmcontrol.h

Abstract:

    Control device (\\.\ReAIVibeBoardVirtualMic) used by the SDK's Windows
    VirtualMic sender: writes of little-endian S16 PCM (16 kHz mono) are
    appended to the PCM feed that the WaveRT capture pin drains.

--*/

#ifndef _REAIVBMIC_VBMCONTROL_H_
#define _REAIVBMIC_VBMCONTROL_H_

#include <portcls.h>

//
// Device/symlink base name; user-mode opens \\.\ReAIVibeBoardVirtualMic.
//
#define VBM_CONTROL_DEVICE_NAME     L"\\Device\\ReAIVibeBoardVirtualMic"
#define VBM_CONTROL_SYMLINK_NAME    L"\\DosDevices\\ReAIVibeBoardVirtualMic"

//
// The control device object, or NULL while not created. Dispatch wrappers
// installed in DriverEntry use this to route IRPs either to the control
// handler or back to PortCls.
//
PDEVICE_OBJECT
VbmGetControlDevice
(
    VOID
);

//
// Create the control device + symbolic link and allocate the PCM feed.
// Call once from AddDevice (PASSIVE_LEVEL); idempotent.
//
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VbmCreateControlDevice
(
    _In_ PDRIVER_OBJECT DriverObject
);

//
// Delete the control device + symbolic link and free the PCM feed.
// Call from device removal and driver unload.
//
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VbmDeleteControlDevice
(
    VOID
);

//
// Dispatch wrappers installed over the PortCls handlers in DriverEntry.
// Everything not addressed to the control device is forwarded to
// PcDispatchIrp unchanged.
//
_Dispatch_type_(IRP_MJ_CREATE)
DRIVER_DISPATCH VbmDispatchCreate;

_Dispatch_type_(IRP_MJ_CLEANUP)
DRIVER_DISPATCH VbmDispatchCleanup;

_Dispatch_type_(IRP_MJ_WRITE)
DRIVER_DISPATCH VbmDispatchWrite;

#endif // _REAIVBMIC_VBMCONTROL_H_
