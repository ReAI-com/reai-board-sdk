/*++

Copyright (c) ReAI Team. All Rights Reserved.

Module Name:

    vbmcontrol.cpp

Abstract:

    Implementation of the control device that receives PCM from the SDK.

    The device object is created once per driver instance in AddDevice and
    deleted on device removal / driver unload. IRP_MJ_WRITE is buffered IO:
    the I/O manager copies the user buffer, we append it to the PCM feed
    and complete synchronously - the SDK feeder must never wait on the
    audio engine.

--*/

#include "definitions.h"
#include "pcmfeed.h"
#include "vbmcontrol.h"

// {BBD5E349-E60A-4975-A537-1774D292674E}
// Defined here as a plain constant on purpose: DEFINE_GUID only declares
// unless initguid.h was included first (this driver funnels its GUID
// definitions through adapter.cpp's PUT_GUIDS_HERE instead).
static const GUID VBM_CONTROL_DEVICE_CLASS = {
    0xbbd5e349, 0xe60a, 0x4975, { 0xa5, 0x37, 0x17, 0x74, 0xd2, 0x92, 0x67, 0x4e }
};

static PDEVICE_OBJECT g_VbmControlDevice = NULL;

static DECLARE_CONST_UNICODE_STRING(VbmControlDeviceName, VBM_CONTROL_DEVICE_NAME);
static DECLARE_CONST_UNICODE_STRING(VbmControlSymlinkName, VBM_CONTROL_SYMLINK_NAME);

// System and administrators get full access; regular users may read and
// write so a non-elevated SDK process can feed the microphone. Nobody gets
// execute, and the DACL is protected (P).
static DECLARE_CONST_UNICODE_STRING(VbmControlSddl,
    L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;WD)");

//=============================================================================
#pragma code_seg()
PDEVICE_OBJECT
VbmGetControlDevice
(
    VOID
)
/*++

Routine Description:

    Returns the control device object (NULL when not created). Safe at any
    IRQL: dispatch wrappers and PnP guards read it on hot paths.

Arguments:

Return Value:

    Device object or NULL.

--*/
{
    return g_VbmControlDevice;
} // VbmGetControlDevice

//=============================================================================
#pragma code_seg("PAGE")
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VbmCreateControlDevice
(
    _In_ PDRIVER_OBJECT DriverObject
)
/*++

Routine Description:

    Creates the control device, its symbolic link, and the PCM feed.

Arguments:

    DriverObject - driver object.

Return Value:

    NT status code.

--*/
{
    PAGED_CODE();

    NTSTATUS ntStatus;

    if (g_VbmControlDevice != NULL)
    {
        return STATUS_SUCCESS;
    }

    ntStatus = VbmPcmFeedInitialize(VBM_PCM_FEED_RING_BYTES_DEFAULT);
    if (!NT_SUCCESS(ntStatus))
    {
        DPF(D_ERROR, ("VbmPcmFeedInitialize failed, 0x%x", ntStatus));
        return ntStatus;
    }

    //
    // Exclusive = FALSE: the owner may open the device multiple times
    // (e.g. a probe plus the audio worker) without failing.
    //
    ntStatus = IoCreateDeviceSecure
    (
        DriverObject,
        0,
        (PUNICODE_STRING)&VbmControlDeviceName,
        FILE_DEVICE_SOUND,
        FILE_DEVICE_SECURE_OPEN,
        FALSE,
        (PUNICODE_STRING)&VbmControlSddl,
        (LPCGUID)&VBM_CONTROL_DEVICE_CLASS,
        &g_VbmControlDevice
    );
    if (!NT_SUCCESS(ntStatus))
    {
        DPF(D_ERROR, ("IoCreateDeviceSecure failed, 0x%x", ntStatus));
        VbmPcmFeedCleanup();
        return ntStatus;
    }

    //
    // Buffered IO: IRP_MJ_WRITE gets Irp->AssociatedIrp.SystemBuffer with
    // the caller's bytes already copied (writes are small, a few KB at most).
    //
    g_VbmControlDevice->Flags |= DO_BUFFERED_IO;
    g_VbmControlDevice->Flags &= ~DO_DEVICE_INITIALIZING;

    ntStatus = IoCreateSymbolicLink
    (
        (PUNICODE_STRING)&VbmControlSymlinkName,
        (PUNICODE_STRING)&VbmControlDeviceName
    );
    if (!NT_SUCCESS(ntStatus))
    {
        DPF(D_ERROR, ("IoCreateSymbolicLink failed, 0x%x", ntStatus));
        IoDeleteDevice(g_VbmControlDevice);
        g_VbmControlDevice = NULL;
        VbmPcmFeedCleanup();
        return ntStatus;
    }

    return STATUS_SUCCESS;
} // VbmCreateControlDevice

//=============================================================================
#pragma code_seg("PAGE")
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VbmDeleteControlDevice
(
    VOID
)
/*++

Routine Description:

    Deletes the control device, its symbolic link, and the PCM feed.

Arguments:

Return Value:

--*/
{
    PAGED_CODE();

    if (g_VbmControlDevice != NULL)
    {
        IoDeleteSymbolicLink((PUNICODE_STRING)&VbmControlSymlinkName);
        IoDeleteDevice(g_VbmControlDevice);
        g_VbmControlDevice = NULL;
    }

    VbmPcmFeedCleanup();
} // VbmDeleteControlDevice

//=============================================================================
#pragma code_seg("PAGE")
_Dispatch_type_(IRP_MJ_CREATE)
NTSTATUS
VbmDispatchCreate
(
    _In_ PDEVICE_OBJECT DeviceObject,
    _Inout_ PIRP Irp
)
/*++

Routine Description:

    Opens of the control device succeed unconditionally; anything else is
    a filter/FDO create and goes to PortCls.

Arguments:

    DeviceObject - target device object.
    Irp - the create IRP.

Return Value:

    NT status code.

--*/
{
    PAGED_CODE();

    if (DeviceObject != g_VbmControlDevice)
    {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    Irp->IoStatus.Status = STATUS_SUCCESS;
    Irp->IoStatus.Information = 0;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);

    return STATUS_SUCCESS;
} // VbmDispatchCreate

//=============================================================================
#pragma code_seg("PAGE")
_Dispatch_type_(IRP_MJ_CLEANUP)
NTSTATUS
VbmDispatchCleanup
(
    _In_ PDEVICE_OBJECT DeviceObject,
    _Inout_ PIRP Irp
)
/*++

Routine Description:

    Cleanup of the control device is a no-op; anything else goes to
    PortCls.

Arguments:

    DeviceObject - target device object.
    Irp - the cleanup IRP.

Return Value:

    NT status code.

--*/
{
    PAGED_CODE();

    if (DeviceObject != g_VbmControlDevice)
    {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    Irp->IoStatus.Status = STATUS_SUCCESS;
    Irp->IoStatus.Information = 0;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);

    return STATUS_SUCCESS;
} // VbmDispatchCleanup

//=============================================================================
#pragma code_seg("PAGE")
_Dispatch_type_(IRP_MJ_WRITE)
NTSTATUS
VbmDispatchWrite
(
    _In_ PDEVICE_OBJECT DeviceObject,
    _Inout_ PIRP Irp
)
/*++

Routine Description:

    Appends the caller's PCM bytes to the feed. The feeder is real-time
    (BLE-paced), so a full ring drops the incoming remainder instead of
    queueing latency; the completion reports how many bytes were accepted.

    Writes aimed at any other device object go to PortCls.

Arguments:

    DeviceObject - target device object.
    Irp - the write IRP.

Return Value:

    NT status code.

--*/
{
    PAGED_CODE();

    if (DeviceObject != g_VbmControlDevice)
    {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    PIO_STACK_LOCATION stack = IoGetCurrentIrpStackLocation(Irp);
    ULONG length = stack->Parameters.Write.Length;
    PVOID systemBuffer = Irp->AssociatedIrp.SystemBuffer;

    NTSTATUS ntStatus = STATUS_SUCCESS;
    ULONG accepted = 0;

    if (length > 0 && systemBuffer == NULL)
    {
        ntStatus = STATUS_INVALID_USER_BUFFER;
    }
    else
    {
        accepted = VbmPcmFeedWrite(systemBuffer, length);
    }

    Irp->IoStatus.Status = ntStatus;
    Irp->IoStatus.Information = accepted;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);

    return ntStatus;
} // VbmDispatchWrite
