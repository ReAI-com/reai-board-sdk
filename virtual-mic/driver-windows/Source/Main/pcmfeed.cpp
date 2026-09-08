/*++

Copyright (c) ReAI Team. All Rights Reserved.

Module Name:

    pcmfeed.cpp

Abstract:

    Implementation of the PCM ring feed shared between the control device
    (user-mode writes) and the WaveRT capture stream (DPC drain).

    Monotonic read/write counters make wrap handling trivial: a position in
    the ring is always counter & mask, and the amount of valid data is
    written - read. Both sides take one spin lock for the duration of a
    bounded memcpy; the consumer only ever runs from the notification DPC of
    a single capture stream, so contention is negligible.

--*/

#include "definitions.h"
#include "pcmfeed.h"

#pragma code_seg()
static KSPIN_LOCK   g_FeedLock;
static PUCHAR       g_FeedBuffer       = NULL;
static ULONG        g_FeedSize         = 0;    // power of two
static ULONG        g_FeedMask         = 0;    // g_FeedSize - 1
static ULONGLONG    g_FeedTotalWritten = 0;
static ULONGLONG    g_FeedTotalRead    = 0;

//=============================================================================
#pragma code_seg("PAGE")
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VbmPcmFeedInitialize
(
    _In_ ULONG RingBytes
)
/*++

Routine Description:

    Allocate the non-paged ring buffer.

Arguments:

    RingBytes - ring size in bytes; zero selects the default. Rounded down
                to a power of two.

Return Value:

    NT status code.

--*/
{
    PAGED_CODE();

    if (g_FeedBuffer != NULL)
    {
        return STATUS_SUCCESS;
    }

    if (RingBytes == 0)
    {
        RingBytes = VBM_PCM_FEED_RING_BYTES_DEFAULT;
    }

    ULONG size = VBM_PCM_FEED_RING_BYTES_DEFAULT;
    while (size > 1 && size / 2 >= RingBytes)
    {
        size /= 2;
    }

    PUCHAR buffer = (PUCHAR)ExAllocatePool2(POOL_FLAG_NON_PAGED, size, REAIVBMIC_POOLTAG);
    if (buffer == NULL)
    {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    KeInitializeSpinLock(&g_FeedLock);
    g_FeedBuffer = buffer;
    g_FeedSize = size;
    g_FeedMask = size - 1;
    g_FeedTotalWritten = 0;
    g_FeedTotalRead = 0;

    return STATUS_SUCCESS;
} // VbmPcmFeedInitialize

//=============================================================================
#pragma code_seg("PAGE")
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VbmPcmFeedCleanup
(
    VOID
)
/*++

Routine Description:

    Free the ring buffer.

Arguments:

Return Value:

--*/
{
    PAGED_CODE();

    if (g_FeedBuffer != NULL)
    {
        ExFreePoolWithTag(g_FeedBuffer, REAIVBMIC_POOLTAG);
        g_FeedBuffer = NULL;
        g_FeedSize = 0;
        g_FeedMask = 0;
        g_FeedTotalWritten = 0;
        g_FeedTotalRead = 0;
    }
} // VbmPcmFeedCleanup

//=============================================================================
#pragma code_seg()
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VbmPcmFeedReset
(
    VOID
)
/*++

Routine Description:

    Drop all buffered audio.

Arguments:

Return Value:

--*/
{
    KIRQL oldIrql;

    KeAcquireSpinLock(&g_FeedLock, &oldIrql);
    g_FeedTotalRead = g_FeedTotalWritten;
    KeReleaseSpinLock(&g_FeedLock, oldIrql);
} // VbmPcmFeedReset

//=============================================================================
#pragma code_seg()
_IRQL_requires_max_(PASSIVE_LEVEL)
ULONG
VbmPcmFeedWrite
(
    _In_reads_bytes_(Length) const VOID* Data,
    _In_ ULONG Length
)
/*++

Routine Description:

    Buffer PCM bytes from the control device. Full-buffer overflow drops
    the incoming remainder instead of accumulating latency.

Arguments:

    Data - little-endian S16 samples, 16 kHz mono.
    Length - byte count.

Return Value:

    Number of bytes accepted.

--*/
{
    KIRQL oldIrql;
    ULONG accepted = 0;

    if (g_FeedBuffer == NULL || Length == 0)
    {
        return 0;
    }

    KeAcquireSpinLock(&g_FeedLock, &oldIrql);

    ULONGLONG buffered = g_FeedTotalWritten - g_FeedTotalRead;
    if (buffered < (ULONGLONG)g_FeedSize)
    {
        ULONG available = g_FeedSize - (ULONG)buffered;
        accepted = (Length < available) ? Length : available;

        ULONG pos = (ULONG)(g_FeedTotalWritten & g_FeedMask);
        ULONG first = min(accepted, g_FeedSize - pos);
        RtlCopyMemory(g_FeedBuffer + pos, Data, first);
        if (accepted > first)
        {
            RtlCopyMemory(g_FeedBuffer, (PUCHAR)Data + first, accepted - first);
        }

        g_FeedTotalWritten += accepted;
    }

    KeReleaseSpinLock(&g_FeedLock, oldIrql);

    return accepted;
} // VbmPcmFeedWrite

//=============================================================================
#pragma code_seg()
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VbmPcmFeedRead
(
    _Out_writes_bytes_(Length) VOID* Dest,
    _In_ ULONG Length
)
/*++

Routine Description:

    Drain PCM bytes into the WaveRT capture buffer, zero-filling any part
    the producer has not supplied (silence on underflow).

Arguments:

    Dest - capture buffer segment.
    Length - byte count.

Return Value:

--*/
{
    KIRQL oldIrql;
    ULONG copied = 0;

    if (g_FeedBuffer != NULL && Length > 0)
    {
        KeAcquireSpinLock(&g_FeedLock, &oldIrql);

        ULONGLONG buffered = g_FeedTotalWritten - g_FeedTotalRead;
        copied = (buffered < (ULONGLONG)Length) ? (ULONG)buffered : Length;

        if (copied > 0)
        {
            ULONG pos = (ULONG)(g_FeedTotalRead & g_FeedMask);
            ULONG first = min(copied, g_FeedSize - pos);
            RtlCopyMemory(Dest, g_FeedBuffer + pos, first);
            if (copied > first)
            {
                RtlCopyMemory((PUCHAR)Dest + first, g_FeedBuffer, copied - first);
            }
            g_FeedTotalRead += copied;
        }

        KeReleaseSpinLock(&g_FeedLock, oldIrql);
    }

    if (copied < Length)
    {
        RtlZeroMemory((PUCHAR)Dest + copied, Length - copied);
    }
} // VbmPcmFeedRead
