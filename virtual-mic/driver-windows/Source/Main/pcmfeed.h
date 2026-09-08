/*++

Copyright (c) ReAI Team. All Rights Reserved.

Module Name:

    pcmfeed.h

Abstract:

    Lock-protected PCM ring buffer fed by the control device
    (\\.\ReAIVibeBoardVirtualMic) and drained by the WaveRT capture
    stream's notification DPC. Underflow is rendered as silence.

--*/

#ifndef _REAIVBMIC_PCMFEED_H_
#define _REAIVBMIC_PCMFEED_H_

#include <portcls.h>

//
// Default ring size in bytes: 128 KB ~ 4 s of 16 kHz mono 16-bit PCM.
//
#define VBM_PCM_FEED_RING_BYTES_DEFAULT     0x20000

//
// Allocate the ring. Idempotent; call from AddDevice (PASSIVE_LEVEL).
//
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VbmPcmFeedInitialize
(
    _In_ ULONG RingBytes
);

//
// Free the ring. Call from driver unload / device removal.
//
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VbmPcmFeedCleanup
(
    VOID
);

//
// Drop everything buffered so far. Call when a fresh capture stream opens
// so stale audio from a previous session is never replayed. Callable at
// DISPATCH_LEVEL.
//
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VbmPcmFeedReset
(
    VOID
);

//
// Producer side (control-device IRP_MJ_WRITE, PASSIVE_LEVEL). Accepts raw
// little-endian S16 samples at the device's native 16 kHz mono rate and
// returns the number of bytes actually buffered; overflow drops the
// remainder (never blocks the user-mode feeder).
//
_IRQL_requires_max_(PASSIVE_LEVEL)
ULONG
VbmPcmFeedWrite
(
    _In_reads_bytes_(Length) const VOID* Data,
    _In_ ULONG Length
);

//
// Consumer side (capture-stream DPC, DISPATCH_LEVEL). Copies buffered
// bytes out and zero-fills anything the producer has not supplied yet.
//
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VbmPcmFeedRead
(
    _Out_writes_bytes_(Length) VOID* Dest,
    _In_ ULONG Length
);

#endif // _REAIVBMIC_PCMFEED_H_
