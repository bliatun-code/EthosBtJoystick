/*
 * Ethos BT Joystick - virtual HID joystick (UMDF 2 HID minidriver)
 *
 * The driver exposes two HID top-level collections:
 *
 *   1. Generic Desktop / Joystick  (input report 1): 8 axes (16 bit) + 32 buttons.
 *      This is what DirectInput / simulators see.
 *   2. Vendor defined 0xFF00 / 1   (output report 2): same 20 byte payload.
 *      The Ethos BT Joystick app writes this report; the driver stores the
 *      payload and hands it back to hidclass as joystick input report 1.
 *
 * The driver runs entirely in user mode (WUDFHost.exe) below the in-box
 * mshidumdf.sys pass-through driver. No kernel code of our own is involved.
 *
 * The request plumbing is derived from Microsoft's "vhidmini2" UMDF 2 sample
 * (https://github.com/microsoft/Windows-driver-samples).
 *
 * Portions Copyright (C) Microsoft Corporation, All Rights Reserved.
 * Licensed under the Microsoft Public License (MS-PL), see the LICENSE file
 * in this directory.
 */

#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS

#include <wdf.h>
#include <hidport.h>

/* ------------------------------------------------------------------------- */
/* Device identity and report layout (keep in sync with app/src/joystick.rs) */
/* ------------------------------------------------------------------------- */

#define EJ_VENDOR_ID            0x1209
#define EJ_PRODUCT_ID           0xE7B0
#define EJ_VERSION              0x0110      /* 1.1 - keep in sync with DRIVER_VERSION in app/src/joystick.rs */

/*
 * Watchdog: if the app has not written a report for this long (it crashed or
 * was killed), the joystick goes back to neutral. A running app writes at
 * least a keep-alive every few hundred milliseconds.
 */
#define EJ_WATCHDOG_MS          1000

#define EJ_REPORT_ID_JOYSTICK   0x01
#define EJ_REPORT_ID_CONTROL    0x02

#define EJ_AXIS_COUNT           8
#define EJ_BUTTON_COUNT         32
#define EJ_PAYLOAD_SIZE         (EJ_AXIS_COUNT * 2 + EJ_BUTTON_COUNT / 8)   /* 20 */
#define EJ_REPORT_SIZE          (1 + EJ_PAYLOAD_SIZE)                       /* 21 */

#define EJ_AXIS_MAX             0x7FFF
#define EJ_AXIS_CENTER          0x4000

#define EJ_MANUFACTURER_STRING  L"Ethos BT Joystick"
#define EJ_PRODUCT_STRING       L"Ethos BT Joystick"
#define EJ_SERIAL_STRING        L"EBJ0001"

static const UCHAR G_ReportDescriptor[] = {
    /* ---- Joystick: what the simulator reads ---- */
    0x05, 0x01,                     /* USAGE_PAGE (Generic Desktop)          */
    0x09, 0x04,                     /* USAGE (Joystick)                      */
    0xA1, 0x01,                     /* COLLECTION (Application)              */
    0x85, EJ_REPORT_ID_JOYSTICK,    /*   REPORT_ID (1)                       */
    0x05, 0x01,                     /*   USAGE_PAGE (Generic Desktop)        */
    0x09, 0x30,                     /*   USAGE (X)                           */
    0x09, 0x31,                     /*   USAGE (Y)                           */
    0x09, 0x32,                     /*   USAGE (Z)                           */
    0x09, 0x33,                     /*   USAGE (Rx)                          */
    0x09, 0x34,                     /*   USAGE (Ry)                          */
    0x09, 0x35,                     /*   USAGE (Rz)                          */
    0x09, 0x36,                     /*   USAGE (Slider)                      */
    0x09, 0x37,                     /*   USAGE (Dial)                        */
    0x15, 0x00,                     /*   LOGICAL_MINIMUM (0)                 */
    0x26, 0xFF, 0x7F,               /*   LOGICAL_MAXIMUM (32767)             */
    0x75, 0x10,                     /*   REPORT_SIZE (16)                    */
    0x95, EJ_AXIS_COUNT,            /*   REPORT_COUNT (8)                    */
    0x81, 0x02,                     /*   INPUT (Data,Var,Abs)                */
    0x05, 0x09,                     /*   USAGE_PAGE (Button)                 */
    0x19, 0x01,                     /*   USAGE_MINIMUM (Button 1)            */
    0x29, EJ_BUTTON_COUNT,          /*   USAGE_MAXIMUM (Button 32)           */
    0x15, 0x00,                     /*   LOGICAL_MINIMUM (0)                 */
    0x25, 0x01,                     /*   LOGICAL_MAXIMUM (1)                 */
    0x75, 0x01,                     /*   REPORT_SIZE (1)                     */
    0x95, EJ_BUTTON_COUNT,          /*   REPORT_COUNT (32)                   */
    0x81, 0x02,                     /*   INPUT (Data,Var,Abs)                */
    0xC0,                           /* END_COLLECTION                        */

    /* ---- Control: what the app writes ---- */
    0x06, 0x00, 0xFF,               /* USAGE_PAGE (Vendor Defined 0xFF00)    */
    0x09, 0x01,                     /* USAGE (Vendor Usage 1)                */
    0xA1, 0x01,                     /* COLLECTION (Application)              */
    0x85, EJ_REPORT_ID_CONTROL,     /*   REPORT_ID (2)                       */
    0x09, 0x02,                     /*   USAGE (Vendor Usage 2)              */
    0x15, 0x00,                     /*   LOGICAL_MINIMUM (0)                 */
    0x26, 0xFF, 0x00,               /*   LOGICAL_MAXIMUM (255)               */
    0x75, 0x08,                     /*   REPORT_SIZE (8)                     */
    0x95, EJ_PAYLOAD_SIZE,          /*   REPORT_COUNT (20)                   */
    0x91, 0x02,                     /*   OUTPUT (Data,Var,Abs)               */
    0xC0                            /* END_COLLECTION                        */
};

static const HID_DESCRIPTOR G_HidDescriptor = {
    0x09,                           /* bLength                               */
    0x21,                           /* bDescriptorType (HID)                 */
    0x0111,                         /* bcdHID                                */
    0x00,                           /* bCountry                              */
    0x01,                           /* bNumDescriptors                       */
    { 0x22, sizeof(G_ReportDescriptor) }
};

/* ------------------------------------------------------------------------- */

typedef struct _DEVICE_CONTEXT {
    WDFDEVICE               Device;
    WDFQUEUE                DefaultQueue;
    WDFQUEUE                ManualQueue;        /* pending IOCTL_HID_READ_REPORT */
    WDFWAITLOCK             Lock;               /* guards State / Dirty          */
    WDFTIMER                Watchdog;           /* neutralises a silent joystick */
    HID_DEVICE_ATTRIBUTES   HidDeviceAttributes;
    UCHAR                   State[EJ_PAYLOAD_SIZE];
    BOOLEAN                 Dirty;              /* State not yet reported        */
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, GetDeviceContext);

DRIVER_INITIALIZE                   DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD           EvtDeviceAdd;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL  EvtIoDeviceControl;
EVT_WDF_TIMER                       EvtWatchdogTimer;

/* ------------------------------------------------------------------------- */
/* Helpers                                                                   */
/* ------------------------------------------------------------------------- */

static VOID
SetNeutralState(
    _Out_writes_bytes_(EJ_PAYLOAD_SIZE) PUCHAR State
    )
{
    ULONG i;

    RtlZeroMemory(State, EJ_PAYLOAD_SIZE);
    for (i = 0; i < EJ_AXIS_COUNT; i++) {
        State[i * 2]     = (UCHAR)(EJ_AXIS_CENTER & 0xFF);
        State[i * 2 + 1] = (UCHAR)(EJ_AXIS_CENTER >> 8);
    }
}

static NTSTATUS
RequestCopyFromBuffer(
    _In_ WDFREQUEST Request,
    _In_reads_bytes_(NumBytesToCopyFrom) const VOID *SourceBuffer,
    _In_ size_t     NumBytesToCopyFrom
    )
{
    NTSTATUS    status;
    WDFMEMORY   memory;
    size_t      outputBufferLength;

    status = WdfRequestRetrieveOutputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WdfMemoryGetBuffer(memory, &outputBufferLength);
    if (outputBufferLength < NumBytesToCopyFrom) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    status = WdfMemoryCopyFromBuffer(memory, 0, (PVOID)SourceBuffer, NumBytesToCopyFrom);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WdfRequestSetInformation(Request, NumBytesToCopyFrom);
    return status;
}

/*
 * mshidumdf.sys splits HID_XFER_PACKET (which has an embedded pointer) into
 * two buffers for UMDF. For "read from device" requests the report id is in
 * the input buffer and the report goes into the output buffer.
 */
static NTSTATUS
RequestGetHidXferPacket_ToReadFromDevice(
    _In_  WDFREQUEST        Request,
    _Out_ HID_XFER_PACKET  *Packet
    )
{
    NTSTATUS    status;
    WDFMEMORY   inputMemory;
    WDFMEMORY   outputMemory;
    size_t      inputBufferLength;
    size_t      outputBufferLength;
    PVOID       inputBuffer;
    PVOID       outputBuffer;

    RtlZeroMemory(Packet, sizeof(*Packet));

    status = WdfRequestRetrieveInputMemory(Request, &inputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    inputBuffer = WdfMemoryGetBuffer(inputMemory, &inputBufferLength);
    if (inputBufferLength < sizeof(UCHAR)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    Packet->reportId = *(PUCHAR)inputBuffer;

    status = WdfRequestRetrieveOutputMemory(Request, &outputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    outputBuffer = WdfMemoryGetBuffer(outputMemory, &outputBufferLength);

    Packet->reportBuffer    = (PUCHAR)outputBuffer;
    Packet->reportBufferLen = (ULONG)outputBufferLength;
    return status;
}

/*
 * For "write to device" requests the report (including its leading report id
 * byte) is in the input buffer.
 */
static NTSTATUS
RequestGetReportToWrite(
    _In_  WDFREQUEST    Request,
    _Out_ PUCHAR       *Report,
    _Out_ size_t       *ReportLength
    )
{
    NTSTATUS    status;
    WDFMEMORY   inputMemory;

    *Report = NULL;
    *ReportLength = 0;

    status = WdfRequestRetrieveInputMemory(Request, &inputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    *Report = (PUCHAR)WdfMemoryGetBuffer(inputMemory, ReportLength);
    return status;
}

static VOID
BuildJoystickReport(
    _In_  PDEVICE_CONTEXT Context,
    _Out_writes_bytes_(EJ_REPORT_SIZE) PUCHAR Report
    )
{
    Report[0] = EJ_REPORT_ID_JOYSTICK;
    RtlCopyMemory(Report + 1, Context->State, EJ_PAYLOAD_SIZE);
}

/* ------------------------------------------------------------------------- */
/* Request handlers                                                          */
/* ------------------------------------------------------------------------- */

/*
 * IOCTL_HID_READ_REPORT: hidclass keeps one of these pending at all times.
 * If there is unreported state we answer immediately, otherwise the request
 * is parked in the manual queue until the app writes new data.
 */
static NTSTATUS
ReadReport(
    _In_  PDEVICE_CONTEXT   Context,
    _In_  WDFREQUEST        Request,
    _Out_ BOOLEAN          *CompleteRequest
    )
{
    NTSTATUS    status;
    UCHAR       report[EJ_REPORT_SIZE];

    WdfWaitLockAcquire(Context->Lock, NULL);

    if (Context->Dirty) {
        BuildJoystickReport(Context, report);
        Context->Dirty = FALSE;
        WdfWaitLockRelease(Context->Lock);

        *CompleteRequest = TRUE;
        return RequestCopyFromBuffer(Request, report, sizeof(report));
    }

    status = WdfRequestForwardToIoQueue(Request, Context->ManualQueue);
    WdfWaitLockRelease(Context->Lock);

    *CompleteRequest = NT_SUCCESS(status) ? FALSE : TRUE;
    return status;
}

/*
 * Makes `State` the joystick's state. If it differs from the current one it is
 * handed to hidclass, either by completing the pending read or - if there is
 * none right now - by marking it as not yet reported.
 */
static VOID
ApplyState(
    _In_ PDEVICE_CONTEXT    Context,
    _In_reads_bytes_(EJ_PAYLOAD_SIZE) const UCHAR *State
    )
{
    WDFREQUEST  readRequest = NULL;
    UCHAR       report[EJ_REPORT_SIZE];

    WdfWaitLockAcquire(Context->Lock, NULL);

    if (RtlEqualMemory(Context->State, State, EJ_PAYLOAD_SIZE)) {
        /* nothing changed (e.g. a keep-alive): no new input report */
        WdfWaitLockRelease(Context->Lock);
        return;
    }

    RtlCopyMemory(Context->State, State, EJ_PAYLOAD_SIZE);
    BuildJoystickReport(Context, report);

    if (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(Context->ManualQueue, &readRequest))) {
        Context->Dirty = FALSE;
    } else {
        readRequest = NULL;
        Context->Dirty = TRUE;
    }

    WdfWaitLockRelease(Context->Lock);

    if (readRequest != NULL) {
        NTSTATUS readStatus = RequestCopyFromBuffer(readRequest, report, sizeof(report));
        WdfRequestComplete(readRequest, readStatus);
    }
}

/*
 * New axis/button data from the app (output report 2, feature or write).
 */
static NTSTATUS
WriteControlReport(
    _In_ PDEVICE_CONTEXT    Context,
    _In_ WDFREQUEST         Request
    )
{
    NTSTATUS    status;
    PUCHAR      buffer;
    size_t      length;

    status = RequestGetReportToWrite(Request, &buffer, &length);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (length < EJ_REPORT_SIZE) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (buffer[0] != EJ_REPORT_ID_CONTROL) {
        return STATUS_INVALID_PARAMETER;
    }

    ApplyState(Context, buffer + 1);

    /* (Re)arm the watchdog: every report from the app proves it is alive. */
    WdfTimerStart(Context->Watchdog, WDF_REL_TIMEOUT_IN_MS(EJ_WATCHDOG_MS));

    WdfRequestSetInformation(Request, EJ_REPORT_SIZE);
    return STATUS_SUCCESS;
}

/*
 * The app has been silent for EJ_WATCHDOG_MS: centre the axes and release the
 * buttons, so a crashed app cannot leave the sticks deflected.
 */
VOID
EvtWatchdogTimer(
    _In_ WDFTIMER Timer
    )
{
    PDEVICE_CONTEXT context = GetDeviceContext((WDFDEVICE)WdfTimerGetParentObject(Timer));
    UCHAR           neutral[EJ_PAYLOAD_SIZE];

    SetNeutralState(neutral);
    ApplyState(context, neutral);
}

/*
 * HidD_GetInputReport for the joystick collection: return the current state.
 */
static NTSTATUS
GetInputReport(
    _In_ PDEVICE_CONTEXT    Context,
    _In_ WDFREQUEST         Request
    )
{
    NTSTATUS        status;
    HID_XFER_PACKET packet;

    status = RequestGetHidXferPacket_ToReadFromDevice(Request, &packet);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (packet.reportId != EJ_REPORT_ID_JOYSTICK) {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen < EJ_REPORT_SIZE) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    WdfWaitLockAcquire(Context->Lock, NULL);
    BuildJoystickReport(Context, packet.reportBuffer);
    WdfWaitLockRelease(Context->Lock);

    WdfRequestSetInformation(Request, EJ_REPORT_SIZE);
    return STATUS_SUCCESS;
}

static NTSTATUS
GetString(
    _In_ WDFREQUEST Request
    )
{
    NTSTATUS    status;
    WDFMEMORY   inputMemory;
    size_t      inputBufferLength;
    PVOID       inputBuffer;
    ULONG       stringId;

    status = WdfRequestRetrieveInputMemory(Request, &inputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    inputBuffer = WdfMemoryGetBuffer(inputMemory, &inputBufferLength);
    if (inputBufferLength < sizeof(ULONG)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /* low word: string id, high word: language id (ignored) */
    stringId = (*(PULONG)inputBuffer) & 0xFFFF;

    switch (stringId) {
    case HID_STRING_ID_IMANUFACTURER:
        return RequestCopyFromBuffer(Request, EJ_MANUFACTURER_STRING, sizeof(EJ_MANUFACTURER_STRING));
    case HID_STRING_ID_IPRODUCT:
        return RequestCopyFromBuffer(Request, EJ_PRODUCT_STRING, sizeof(EJ_PRODUCT_STRING));
    case HID_STRING_ID_ISERIALNUMBER:
        return RequestCopyFromBuffer(Request, EJ_SERIAL_STRING, sizeof(EJ_SERIAL_STRING));
    default:
        return STATUS_INVALID_PARAMETER;
    }
}

VOID
EvtIoDeviceControl(
    _In_ WDFQUEUE   Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t     OutputBufferLength,
    _In_ size_t     InputBufferLength,
    _In_ ULONG      IoControlCode
    )
{
    NTSTATUS        status;
    BOOLEAN         completeRequest = TRUE;
    PDEVICE_CONTEXT context = GetDeviceContext(WdfIoQueueGetDevice(Queue));

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
        status = RequestCopyFromBuffer(Request, &G_HidDescriptor, G_HidDescriptor.bLength);
        break;

    case IOCTL_HID_GET_DEVICE_ATTRIBUTES:
        status = RequestCopyFromBuffer(Request,
                                       &context->HidDeviceAttributes,
                                       sizeof(HID_DEVICE_ATTRIBUTES));
        break;

    case IOCTL_HID_GET_REPORT_DESCRIPTOR:
        status = RequestCopyFromBuffer(Request, G_ReportDescriptor, sizeof(G_ReportDescriptor));
        break;

    case IOCTL_HID_READ_REPORT:
        status = ReadReport(context, Request, &completeRequest);
        break;

    case IOCTL_HID_WRITE_REPORT:
    case IOCTL_UMDF_HID_SET_OUTPUT_REPORT:
    case IOCTL_UMDF_HID_SET_FEATURE:
        status = WriteControlReport(context, Request);
        break;

    case IOCTL_UMDF_HID_GET_INPUT_REPORT:
        status = GetInputReport(context, Request);
        break;

    case IOCTL_HID_GET_STRING:
        status = GetString(Request);
        break;

    default:
        /*
         * IOCTL_HID_ACTIVATE_DEVICE, IOCTL_HID_DEACTIVATE_DEVICE,
         * IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST, IOCTL_UMDF_HID_GET_FEATURE,
         * IOCTL_HID_GET_INDEXED_STRING, ... are not needed for a virtual device.
         */
        status = STATUS_NOT_IMPLEMENTED;
        break;
    }

    if (completeRequest) {
        WdfRequestComplete(Request, status);
    }
}

/* ------------------------------------------------------------------------- */
/* Device / driver setup                                                     */
/* ------------------------------------------------------------------------- */

NTSTATUS
EvtDeviceAdd(
    _In_    WDFDRIVER       Driver,
    _Inout_ PWDFDEVICE_INIT DeviceInit
    )
{
    NTSTATUS                status;
    WDF_OBJECT_ATTRIBUTES   attributes;
    WDF_IO_QUEUE_CONFIG     queueConfig;
    WDF_TIMER_CONFIG        timerConfig;
    WDFDEVICE               device;
    PDEVICE_CONTEXT         context;

    UNREFERENCED_PARAMETER(Driver);

    /*
     * mshidumdf.sys is the function driver; we sit below it as a filter,
     * which also relinquishes power policy ownership to hidclass.
     */
    WdfFdoInitSetFilter(DeviceInit);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DEVICE_CONTEXT);

    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    context = GetDeviceContext(device);
    context->Device = device;
    context->Dirty  = TRUE;     /* report the neutral state on the first read */
    SetNeutralState(context->State);

    RtlZeroMemory(&context->HidDeviceAttributes, sizeof(HID_DEVICE_ATTRIBUTES));
    context->HidDeviceAttributes.Size          = sizeof(HID_DEVICE_ATTRIBUTES);
    context->HidDeviceAttributes.VendorID      = EJ_VENDOR_ID;
    context->HidDeviceAttributes.ProductID     = EJ_PRODUCT_ID;
    context->HidDeviceAttributes.VersionNumber = EJ_VERSION;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = device;
    status = WdfWaitLockCreate(&attributes, &context->Lock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* One-shot watchdog, restarted by every report from the app. */
    WDF_TIMER_CONFIG_INIT(&timerConfig, EvtWatchdogTimer);
    timerConfig.AutomaticSerialization = FALSE;     /* ApplyState takes our own lock */
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = device;
    status = WdfTimerCreate(&timerConfig, &attributes, &context->Watchdog);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /*
     * Default parallel queue: hidclass' internal IOCTLs arrive here as
     * ordinary device controls (mshidumdf.sys converts them).
     */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoDeviceControl = EvtIoDeviceControl;

    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &context->DefaultQueue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Manual queue holding the pending read request(s) from hidclass. */
    WDF_IO_QUEUE_CONFIG_INIT(&queueConfig, WdfIoQueueDispatchManual);

    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &context->ManualQueue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    return STATUS_SUCCESS;
}

NTSTATUS
DriverEntry(
    _In_ PDRIVER_OBJECT     DriverObject,
    _In_ PUNICODE_STRING    RegistryPath
    )
{
    WDF_DRIVER_CONFIG config;

    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);

    return WdfDriverCreate(DriverObject,
                           RegistryPath,
                           WDF_NO_OBJECT_ATTRIBUTES,
                           &config,
                           WDF_NO_HANDLE);
}
