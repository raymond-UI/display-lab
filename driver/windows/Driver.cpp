/*++

Display Lab indirect display driver.

Based on Microsoft's IddSampleDriver (Windows-driver-samples,
video/IndirectDisplay), Copyright (c) Microsoft Corporation, used under the
Microsoft Public License (LICENSE-MS-PL in this folder).

What changed from the sample:

  * No monitors at start-up. The lab plugs each one over a named pipe,
    with its own EDID (so Windows sees its own manufacturer, product,
    serial and name) and its own list of modes.
  * Unplugging reports the departure of that one monitor. The others are
    not touched, so a cable pull on one screen never blinks the rest.
  * No WPP tracing; the pipe replies say what happened.

The pipe, \\.\pipe\DisplayLabIdd, takes one ASCII line per connection and
answers one line, "OK ..." or "ERR <reason>":

  PING                              -> OK displaylab-idd 1
  PLUG <slot> <edid hex> <modes>    -> OK
       modes: WxH@MILLIHZ[,WxH@MILLIHZ...], the first is preferred
  UNPLUG <slot>                     -> OK
  LIST                              -> OK <used slots, comma separated>

Environment:

    User Mode, UMDF

--*/

#include "Driver.h"

#include <sddl.h>

#include <algorithm>
#include <cstdio>
#include <cstring>

using namespace std;
using namespace DisplayLab;
using namespace Microsoft::WRL;

static constexpr wchar_t PipeName[] = L"\\\\.\\pipe\\DisplayLabIdd";
static constexpr DWORD MaxRequest = 4096;

// The one device context, for the IddCx callbacks that are not handed one.
static IndirectDeviceContext* g_Context = nullptr;

#pragma region helpers

static inline void FillSignalInfo(DISPLAYCONFIG_VIDEO_SIGNAL_INFO& Signal, const Mode& M, bool bMonitorMode)
{
    Signal.totalSize.cx = Signal.activeSize.cx = M.Width;
    Signal.totalSize.cy = Signal.activeSize.cy = M.Height;

    // See https://docs.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_video_signal_info
    Signal.AdditionalSignalInfo.vSyncFreqDivider = bMonitorMode ? 0 : 1;
    Signal.AdditionalSignalInfo.videoStandard = 255;

    Signal.vSyncFreq.Numerator = M.RefreshMilliHz;
    Signal.vSyncFreq.Denominator = 1000;
    Signal.hSyncFreq.Numerator = M.RefreshMilliHz * M.Height;
    Signal.hSyncFreq.Denominator = 1000;

    Signal.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;

    Signal.pixelRate = ((UINT64)M.RefreshMilliHz) * ((UINT64)M.Width) * ((UINT64)M.Height) / 1000;
}

static IDDCX_MONITOR_MODE MonitorMode(const Mode& M, IDDCX_MONITOR_MODE_ORIGIN Origin)
{
    IDDCX_MONITOR_MODE Out = {};
    Out.Size = sizeof(Out);
    Out.Origin = Origin;
    FillSignalInfo(Out.MonitorVideoSignalInfo, M, true);
    return Out;
}

static IDDCX_TARGET_MODE TargetMode(const Mode& M)
{
    IDDCX_TARGET_MODE Out = {};
    Out.Size = sizeof(Out);
    FillSignalInfo(Out.TargetVideoSignalInfo.targetVideoSignalInfo, M, false);
    return Out;
}

// A stable container id for an EDID, so Windows treats the same identity as
// the same monitor across plugs (FNV-1a, folded into 128 bits).
static GUID ContainerIdFor(const vector<BYTE>& Edid)
{
    UINT64 a = 14695981039346656037ull;
    UINT64 b = 1099511628211ull ^ 0x9e3779b97f4a7c15ull;
    for (BYTE byte : Edid)
    {
        a = (a ^ byte) * 1099511628211ull;
        b = (b ^ byte) * 0x100000001b3ull + 0x632be59bd9b4e019ull;
    }
    GUID g;
    memcpy(&g, &a, 8);
    memcpy(reinterpret_cast<BYTE*>(&g) + 8, &b, 8);
    return g;
}

static bool ParseHex(const string& Hex, vector<BYTE>& Out)
{
    if (Hex.size() % 2 != 0)
    {
        return false;
    }
    Out.clear();
    for (size_t i = 0; i < Hex.size(); i += 2)
    {
        unsigned int byte = 0;
        if (sscanf_s(Hex.c_str() + i, "%2x", &byte) != 1)
        {
            return false;
        }
        Out.push_back(static_cast<BYTE>(byte));
    }
    return true;
}

static bool ParseModes(const string& Text, vector<Mode>& Out)
{
    Out.clear();
    size_t start = 0;
    while (start < Text.size())
    {
        size_t end = Text.find(',', start);
        string one = Text.substr(start, end == string::npos ? string::npos : end - start);
        Mode M = {};
        unsigned long w = 0, h = 0, mhz = 0;
        if (sscanf_s(one.c_str(), "%lux%lu@%lu", &w, &h, &mhz) != 3 || w == 0 || h == 0 || mhz == 0)
        {
            return false;
        }
        M.Width = w;
        M.Height = h;
        M.RefreshMilliHz = mhz;
        Out.push_back(M);
        if (end == string::npos)
        {
            break;
        }
        start = end + 1;
    }
    return !Out.empty();
}

static string Hex32(NTSTATUS Status)
{
    char buf[16];
    sprintf_s(buf, "0x%08X", static_cast<unsigned int>(Status));
    return buf;
}

#pragma endregion

extern "C" DRIVER_INITIALIZE DriverEntry;

EVT_WDF_DRIVER_DEVICE_ADD IddDeviceAdd;
EVT_WDF_DEVICE_D0_ENTRY IddDeviceD0Entry;

EVT_IDD_CX_ADAPTER_INIT_FINISHED IddAdapterInitFinished;
EVT_IDD_CX_ADAPTER_COMMIT_MODES IddAdapterCommitModes;

EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION IddParseMonitorDescription;
EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES IddMonitorGetDefaultModes;
EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES IddMonitorQueryModes;

EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN IddMonitorAssignSwapChain;
EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN IddMonitorUnassignSwapChain;

struct IndirectDeviceContextWrapper
{
    IndirectDeviceContext* pContext;

    void Cleanup()
    {
        delete pContext;
        pContext = nullptr;
    }
};

struct IndirectMonitorContextWrapper
{
    IndirectMonitorContext* pContext;

    void Cleanup()
    {
        delete pContext;
        pContext = nullptr;
    }
};

WDF_DECLARE_CONTEXT_TYPE(IndirectDeviceContextWrapper);
WDF_DECLARE_CONTEXT_TYPE(IndirectMonitorContextWrapper);

extern "C" BOOL WINAPI DllMain(_In_ HINSTANCE hInstance, _In_ UINT dwReason, _In_opt_ LPVOID lpReserved)
{
    UNREFERENCED_PARAMETER(hInstance);
    UNREFERENCED_PARAMETER(lpReserved);
    UNREFERENCED_PARAMETER(dwReason);
    return TRUE;
}

_Use_decl_annotations_
extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT pDriverObject, PUNICODE_STRING pRegistryPath)
{
    WDF_DRIVER_CONFIG Config;
    WDF_OBJECT_ATTRIBUTES Attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&Attributes);
    WDF_DRIVER_CONFIG_INIT(&Config, IddDeviceAdd);
    return WdfDriverCreate(pDriverObject, pRegistryPath, &Attributes, &Config, WDF_NO_HANDLE);
}

_Use_decl_annotations_
NTSTATUS IddDeviceAdd(WDFDRIVER Driver, PWDFDEVICE_INIT pDeviceInit)
{
    UNREFERENCED_PARAMETER(Driver);

    WDF_PNPPOWER_EVENT_CALLBACKS PnpPowerCallbacks;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&PnpPowerCallbacks);
    PnpPowerCallbacks.EvtDeviceD0Entry = IddDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(pDeviceInit, &PnpPowerCallbacks);

    IDD_CX_CLIENT_CONFIG IddConfig;
    IDD_CX_CLIENT_CONFIG_INIT(&IddConfig);
    IddConfig.EvtIddCxAdapterInitFinished = IddAdapterInitFinished;
    IddConfig.EvtIddCxParseMonitorDescription = IddParseMonitorDescription;
    IddConfig.EvtIddCxMonitorGetDefaultDescriptionModes = IddMonitorGetDefaultModes;
    IddConfig.EvtIddCxMonitorQueryTargetModes = IddMonitorQueryModes;
    IddConfig.EvtIddCxAdapterCommitModes = IddAdapterCommitModes;
    IddConfig.EvtIddCxMonitorAssignSwapChain = IddMonitorAssignSwapChain;
    IddConfig.EvtIddCxMonitorUnassignSwapChain = IddMonitorUnassignSwapChain;

    NTSTATUS Status = IddCxDeviceInitConfig(pDeviceInit, &IddConfig);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    WDF_OBJECT_ATTRIBUTES Attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&Attr, IndirectDeviceContextWrapper);
    Attr.EvtCleanupCallback = [](WDFOBJECT Object)
    {
        auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(Object);
        if (pContext)
        {
            pContext->Cleanup();
        }
    };

    WDFDEVICE Device = nullptr;
    Status = WdfDeviceCreate(&pDeviceInit, &Attr, &Device);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    Status = IddCxDeviceInitialize(Device);

    auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(Device);
    pContext->pContext = new IndirectDeviceContext(Device);

    return Status;
}

_Use_decl_annotations_
NTSTATUS IddDeviceD0Entry(WDFDEVICE Device, WDF_POWER_DEVICE_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);
    auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(Device);
    pContext->pContext->InitAdapter();
    return STATUS_SUCCESS;
}

#pragma region Direct3DDevice

Direct3DDevice::Direct3DDevice(LUID AdapterLuid) : AdapterLuid(AdapterLuid)
{
}

HRESULT Direct3DDevice::Init()
{
    HRESULT hr = CreateDXGIFactory2(0, IID_PPV_ARGS(&DxgiFactory));
    if (FAILED(hr))
    {
        return hr;
    }
    hr = DxgiFactory->EnumAdapterByLuid(AdapterLuid, IID_PPV_ARGS(&Adapter));
    if (FAILED(hr))
    {
        return hr;
    }
    return D3D11CreateDevice(Adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0,
                             D3D11_SDK_VERSION, &Device, nullptr, &DeviceContext);
}

#pragma endregion

#pragma region SwapChainProcessor

SwapChainProcessor::SwapChainProcessor(IDDCX_SWAPCHAIN hSwapChain, shared_ptr<Direct3DDevice> Device, HANDLE NewFrameEvent)
    : m_hSwapChain(hSwapChain), m_Device(Device), m_hAvailableBufferEvent(NewFrameEvent)
{
    m_hTerminateEvent.Attach(CreateEvent(nullptr, FALSE, FALSE, nullptr));
    m_hThread.Attach(CreateThread(nullptr, 0, RunThread, this, 0, nullptr));
}

SwapChainProcessor::~SwapChainProcessor()
{
    SetEvent(m_hTerminateEvent.Get());
    if (m_hThread.Get())
    {
        WaitForSingleObject(m_hThread.Get(), INFINITE);
    }
}

DWORD CALLBACK SwapChainProcessor::RunThread(LPVOID Argument)
{
    reinterpret_cast<SwapChainProcessor*>(Argument)->Run();
    return 0;
}

void SwapChainProcessor::Run()
{
    DWORD AvTask = 0;
    HANDLE AvTaskHandle = AvSetMmThreadCharacteristicsW(L"Distribution", &AvTask);

    RunCore();

    // Deleting the swap chain when the loop ends makes the system hand out a
    // new one if it still needs one.
    WdfObjectDelete((WDFOBJECT)m_hSwapChain);
    m_hSwapChain = nullptr;

    AvRevertMmThreadCharacteristics(AvTaskHandle);
}

void SwapChainProcessor::RunCore()
{
    ComPtr<IDXGIDevice> DxgiDevice;
    HRESULT hr = m_Device->Device.As(&DxgiDevice);
    if (FAILED(hr))
    {
        return;
    }

    IDARG_IN_SWAPCHAINSETDEVICE SetDevice = {};
    SetDevice.pDevice = DxgiDevice.Get();
    hr = IddCxSwapChainSetDevice(m_hSwapChain, &SetDevice);
    if (FAILED(hr))
    {
        return;
    }

    for (;;)
    {
        ComPtr<IDXGIResource> AcquiredBuffer;
        IDARG_OUT_RELEASEANDACQUIREBUFFER Buffer = {};
        hr = IddCxSwapChainReleaseAndAcquireBuffer(m_hSwapChain, &Buffer);

        if (hr == E_PENDING)
        {
            HANDLE WaitHandles[] = {m_hAvailableBufferEvent, m_hTerminateEvent.Get()};
            DWORD WaitResult = WaitForMultipleObjects(ARRAYSIZE(WaitHandles), WaitHandles, FALSE, 16);
            if (WaitResult == WAIT_OBJECT_0 || WaitResult == WAIT_TIMEOUT)
            {
                continue;
            }
            break; // terminate, or the wait failed
        }
        else if (SUCCEEDED(hr))
        {
            // Nothing to do with the frame: the lab captures the desktop.
            AcquiredBuffer.Attach(Buffer.MetaData.pSurface);
            AcquiredBuffer.Reset();
            hr = IddCxSwapChainFinishedProcessingFrame(m_hSwapChain);
            if (FAILED(hr))
            {
                break;
            }
        }
        else
        {
            break; // the swap chain was abandoned
        }
    }
}

#pragma endregion

#pragma region IndirectDeviceContext

IndirectDeviceContext::IndirectDeviceContext(_In_ WDFDEVICE WdfDevice) : m_WdfDevice(WdfDevice)
{
    m_Adapter = {};
}

IndirectDeviceContext::~IndirectDeviceContext()
{
}

void IndirectDeviceContext::InitAdapter()
{
    IDDCX_ADAPTER_CAPS AdapterCaps = {};
    AdapterCaps.Size = sizeof(AdapterCaps);
    AdapterCaps.MaxMonitorsSupported = MaxSlots;
    AdapterCaps.EndPointDiagnostics.Size = sizeof(AdapterCaps.EndPointDiagnostics);
    AdapterCaps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    AdapterCaps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;
    AdapterCaps.EndPointDiagnostics.pEndPointFriendlyName = L"Display Lab virtual screens";
    AdapterCaps.EndPointDiagnostics.pEndPointManufacturerName = L"Display Lab";
    AdapterCaps.EndPointDiagnostics.pEndPointModelName = L"Display Lab IDD";

    IDDCX_ENDPOINT_VERSION Version = {};
    Version.Size = sizeof(Version);
    Version.MajorVer = 1;
    AdapterCaps.EndPointDiagnostics.pFirmwareVersion = &Version;
    AdapterCaps.EndPointDiagnostics.pHardwareVersion = &Version;

    WDF_OBJECT_ATTRIBUTES Attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&Attr, IndirectDeviceContextWrapper);

    IDARG_IN_ADAPTER_INIT AdapterInit = {};
    AdapterInit.WdfDevice = m_WdfDevice;
    AdapterInit.pCaps = &AdapterCaps;
    AdapterInit.ObjectAttributes = &Attr;

    IDARG_OUT_ADAPTER_INIT AdapterInitOut;
    NTSTATUS Status = IddCxAdapterInitAsync(&AdapterInit, &AdapterInitOut);
    if (NT_SUCCESS(Status))
    {
        m_Adapter = AdapterInitOut.AdapterObject;
        auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(AdapterInitOut.AdapterObject);
        pContext->pContext = this;
    }
}

string IndirectDeviceContext::Plug(UINT Index, vector<BYTE> Edid, vector<Mode> Modes)
{
    if (Index >= MaxSlots)
    {
        return "ERR slot out of range";
    }
    if (Edid.size() != 128 && Edid.size() != 256)
    {
        return "ERR an EDID is 128 or 256 bytes";
    }

    // Fill the slot before creating the monitor: IddCx calls back for its
    // modes while IddCxMonitorCreate and IddCxMonitorArrival run, and the
    // lock must not be held across those calls.
    AcquireSRWLockExclusive(&m_Lock);
    if (m_Slots[Index].Used)
    {
        ReleaseSRWLockExclusive(&m_Lock);
        return "ERR slot in use";
    }
    m_Slots[Index].Used = true;
    m_Slots[Index].Monitor = nullptr;
    m_Slots[Index].Edid = move(Edid);
    m_Slots[Index].Modes = move(Modes);
    BYTE* EdidData = m_Slots[Index].Edid.data();
    UINT EdidSize = static_cast<UINT>(m_Slots[Index].Edid.size());
    GUID ContainerId = ContainerIdFor(m_Slots[Index].Edid);
    ReleaseSRWLockExclusive(&m_Lock);

    auto Release = [&](const string& Why)
    {
        AcquireSRWLockExclusive(&m_Lock);
        m_Slots[Index] = Slot{};
        ReleaseSRWLockExclusive(&m_Lock);
        return "ERR " + Why;
    };

    WDF_OBJECT_ATTRIBUTES Attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&Attr, IndirectMonitorContextWrapper);
    Attr.EvtCleanupCallback = [](WDFOBJECT Object)
    {
        auto* pContext = WdfObjectGet_IndirectMonitorContextWrapper(Object);
        if (pContext)
        {
            pContext->Cleanup();
        }
    };

    IDDCX_MONITOR_INFO MonitorInfo = {};
    MonitorInfo.Size = sizeof(MonitorInfo);
    MonitorInfo.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    MonitorInfo.ConnectorIndex = Index;
    MonitorInfo.MonitorDescription.Size = sizeof(MonitorInfo.MonitorDescription);
    MonitorInfo.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    MonitorInfo.MonitorDescription.DataSize = EdidSize;
    MonitorInfo.MonitorDescription.pData = EdidData;
    MonitorInfo.MonitorContainerId = ContainerId;

    IDARG_IN_MONITORCREATE MonitorCreate = {};
    MonitorCreate.ObjectAttributes = &Attr;
    MonitorCreate.pMonitorInfo = &MonitorInfo;

    IDARG_OUT_MONITORCREATE MonitorCreateOut;
    NTSTATUS Status = IddCxMonitorCreate(m_Adapter, &MonitorCreate, &MonitorCreateOut);
    if (!NT_SUCCESS(Status))
    {
        return Release("IddCxMonitorCreate " + Hex32(Status));
    }

    auto* pMonitorContextWrapper = WdfObjectGet_IndirectMonitorContextWrapper(MonitorCreateOut.MonitorObject);
    pMonitorContextWrapper->pContext = new IndirectMonitorContext(MonitorCreateOut.MonitorObject);

    AcquireSRWLockExclusive(&m_Lock);
    m_Slots[Index].Monitor = MonitorCreateOut.MonitorObject;
    ReleaseSRWLockExclusive(&m_Lock);

    IDARG_OUT_MONITORARRIVAL ArrivalOut;
    Status = IddCxMonitorArrival(MonitorCreateOut.MonitorObject, &ArrivalOut);
    if (!NT_SUCCESS(Status))
    {
        WdfObjectDelete((WDFOBJECT)MonitorCreateOut.MonitorObject);
        return Release("IddCxMonitorArrival " + Hex32(Status));
    }
    return "OK";
}

string IndirectDeviceContext::Unplug(UINT Index)
{
    if (Index >= MaxSlots)
    {
        return "ERR slot out of range";
    }
    AcquireSRWLockShared(&m_Lock);
    bool Used = m_Slots[Index].Used;
    IDDCX_MONITOR Monitor = m_Slots[Index].Monitor;
    ReleaseSRWLockShared(&m_Lock);
    if (!Used || !Monitor)
    {
        return "ERR nothing is plugged into that slot";
    }

    NTSTATUS Status = IddCxMonitorDeparture(Monitor);

    AcquireSRWLockExclusive(&m_Lock);
    m_Slots[Index] = Slot{};
    ReleaseSRWLockExclusive(&m_Lock);

    if (!NT_SUCCESS(Status))
    {
        return "ERR IddCxMonitorDeparture " + Hex32(Status);
    }
    return "OK";
}

string IndirectDeviceContext::List()
{
    string Out = "OK";
    bool First = true;
    AcquireSRWLockShared(&m_Lock);
    for (UINT i = 0; i < MaxSlots; i++)
    {
        if (m_Slots[i].Used && m_Slots[i].Monitor)
        {
            Out += First ? " " : ",";
            Out += to_string(i);
            First = false;
        }
    }
    ReleaseSRWLockShared(&m_Lock);
    return Out;
}

bool IndirectDeviceContext::ModesForEdid(const BYTE* Edid, UINT Size, vector<Mode>& Out)
{
    bool Found = false;
    AcquireSRWLockShared(&m_Lock);
    for (const Slot& S : m_Slots)
    {
        if (S.Used && S.Edid.size() == Size && memcmp(S.Edid.data(), Edid, Size) == 0)
        {
            Out = S.Modes;
            Found = true;
            break;
        }
    }
    ReleaseSRWLockShared(&m_Lock);
    return Found;
}

bool IndirectDeviceContext::ModesForMonitor(IDDCX_MONITOR Monitor, vector<Mode>& Out)
{
    bool Found = false;
    AcquireSRWLockShared(&m_Lock);
    for (const Slot& S : m_Slots)
    {
        if (S.Used && S.Monitor == Monitor)
        {
            Out = S.Modes;
            Found = true;
            break;
        }
    }
    ReleaseSRWLockShared(&m_Lock);
    return Found;
}

IndirectMonitorContext::IndirectMonitorContext(_In_ IDDCX_MONITOR Monitor) : m_Monitor(Monitor)
{
}

IndirectMonitorContext::~IndirectMonitorContext()
{
    m_ProcessingThread.reset();
}

void IndirectMonitorContext::AssignSwapChain(IDDCX_SWAPCHAIN SwapChain, LUID RenderAdapter, HANDLE NewFrameEvent)
{
    m_ProcessingThread.reset();
    auto Device = make_shared<Direct3DDevice>(RenderAdapter);
    if (FAILED(Device->Init()))
    {
        // Deleting the swap chain makes the OS try again with a new one.
        WdfObjectDelete(SwapChain);
    }
    else
    {
        m_ProcessingThread.reset(new SwapChainProcessor(SwapChain, Device, NewFrameEvent));
    }
}

void IndirectMonitorContext::UnassignSwapChain()
{
    m_ProcessingThread.reset();
}

#pragma endregion

#pragma region Pipe

static string Handle(IndirectDeviceContext* Context, const string& Line)
{
    char Command[16] = {};
    if (sscanf_s(Line.c_str(), "%15s", Command, static_cast<unsigned>(sizeof(Command))) != 1)
    {
        return "ERR empty request";
    }
    string Cmd = Command;
    if (Cmd == "PING")
    {
        return "OK displaylab-idd 1";
    }
    if (Cmd == "LIST")
    {
        return Context->List();
    }
    if (Cmd == "UNPLUG")
    {
        unsigned int Index = 0;
        if (sscanf_s(Line.c_str(), "%*s %u", &Index) != 1)
        {
            return "ERR usage: UNPLUG <slot>";
        }
        return Context->Unplug(Index);
    }
    if (Cmd == "PLUG")
    {
        unsigned int Index = 0;
        char Hex[1024] = {};
        char Modes[1024] = {};
        if (sscanf_s(Line.c_str(), "%*s %u %1023s %1023s", &Index, Hex, static_cast<unsigned>(sizeof(Hex)), Modes,
                     static_cast<unsigned>(sizeof(Modes))) != 3)
        {
            return "ERR usage: PLUG <slot> <edid hex> <WxH@MILLIHZ,...>";
        }
        vector<BYTE> Edid;
        if (!ParseHex(Hex, Edid))
        {
            return "ERR the EDID is not hex";
        }
        vector<Mode> ModeList;
        if (!ParseModes(Modes, ModeList))
        {
            return "ERR modes are WxH@MILLIHZ, comma separated";
        }
        return Context->Plug(Index, move(Edid), move(ModeList));
    }
    return "ERR unknown command " + Cmd;
}

// Requests are answered one at a time, whichever connection they come on.
static SRWLOCK g_RequestLock = SRWLOCK_INIT;

struct Connection
{
    IndirectDeviceContext* Context;
    HANDLE Pipe;
};

static DWORD WINAPI ConnectionThread(LPVOID Argument)
{
    unique_ptr<Connection> C(static_cast<Connection*>(Argument));
    string Line;
    char Buf[512];
    DWORD Read = 0;
    while (Line.size() < MaxRequest && Line.find('\n') == string::npos && ReadFile(C->Pipe, Buf, sizeof(Buf), &Read, nullptr) &&
           Read > 0)
    {
        Line.append(Buf, Read);
    }
    size_t End = Line.find_first_of("\r\n");
    if (End != string::npos)
    {
        Line.resize(End);
    }
    AcquireSRWLockExclusive(&g_RequestLock);
    string Reply = Handle(C->Context, Line) + "\n";
    ReleaseSRWLockExclusive(&g_RequestLock);
    DWORD Written = 0;
    WriteFile(C->Pipe, Reply.data(), static_cast<DWORD>(Reply.size()), &Written, nullptr);
    FlushFileBuffers(C->Pipe);
    DisconnectNamedPipe(C->Pipe);
    CloseHandle(C->Pipe);
    return 0;
}

static DWORD WINAPI PipeThread(LPVOID Argument)
{
    auto* Context = static_cast<IndirectDeviceContext*>(Argument);

    // SYSTEM and administrators, and whoever is logged on at the console:
    // the lab runs as the signed-in user. Nobody else may plug screens.
    SECURITY_ATTRIBUTES Sa = {};
    Sa.nLength = sizeof(Sa);
    Sa.bInheritHandle = FALSE;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)", SDDL_REVISION_1,
                                                              &Sa.lpSecurityDescriptor, nullptr))
    {
        return 1;
    }

    // Each connection is answered on its own thread, so a new pipe instance
    // is listening again at once: a client never finds the pipe missing.
    for (;;)
    {
        HANDLE Pipe = CreateNamedPipeW(PipeName, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                                       PIPE_UNLIMITED_INSTANCES, MaxRequest, MaxRequest, 0, &Sa);
        if (Pipe == INVALID_HANDLE_VALUE)
        {
            Sleep(100);
            continue;
        }
        BOOL Connected = ConnectNamedPipe(Pipe, nullptr) ? TRUE : (GetLastError() == ERROR_PIPE_CONNECTED);
        if (!Connected)
        {
            CloseHandle(Pipe);
            continue;
        }
        auto* C = new Connection{Context, Pipe};
        HANDLE Thread = CreateThread(nullptr, 0, ConnectionThread, C, 0, nullptr);
        if (Thread)
        {
            CloseHandle(Thread);
        }
        else
        {
            delete C;
            DisconnectNamedPipe(Pipe);
            CloseHandle(Pipe);
        }
    }
}

void DisplayLab::StartPipeServer(IndirectDeviceContext* Context)
{
    static bool Started = false;
    if (Started)
    {
        return;
    }
    Started = true;
    HANDLE Thread = CreateThread(nullptr, 0, PipeThread, Context, 0, nullptr);
    if (Thread)
    {
        CloseHandle(Thread);
    }
}

#pragma endregion

#pragma region DDI Callbacks

_Use_decl_annotations_
NTSTATUS IddAdapterInitFinished(IDDCX_ADAPTER AdapterObject, const IDARG_IN_ADAPTER_INIT_FINISHED* pInArgs)
{
    // The adapter is ready. No monitors yet: the lab plugs them over the pipe.
    auto* pDeviceContextWrapper = WdfObjectGet_IndirectDeviceContextWrapper(AdapterObject);
    if (NT_SUCCESS(pInArgs->AdapterInitStatus))
    {
        g_Context = pDeviceContextWrapper->pContext;
        StartPipeServer(g_Context);
    }
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddAdapterCommitModes(IDDCX_ADAPTER AdapterObject, const IDARG_IN_COMMITMODES* pInArgs)
{
    UNREFERENCED_PARAMETER(AdapterObject);
    UNREFERENCED_PARAMETER(pInArgs);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION* pInArgs, IDARG_OUT_PARSEMONITORDESCRIPTION* pOutArgs)
{
    // The modes come from the lab's PLUG request, not from parsing the EDID:
    // the EDID carries the identity, the request says exactly which modes the
    // screen offers.
    vector<Mode> Modes;
    if (!g_Context ||
        !g_Context->ModesForEdid(static_cast<const BYTE*>(pInArgs->MonitorDescription.pData), pInArgs->MonitorDescription.DataSize, Modes))
    {
        return STATUS_INVALID_PARAMETER;
    }

    pOutArgs->MonitorModeBufferOutputCount = static_cast<UINT>(Modes.size());
    if (pInArgs->MonitorModeBufferInputCount < Modes.size())
    {
        // No buffer: the caller only asked how many.
        return (pInArgs->MonitorModeBufferInputCount > 0) ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    }
    for (size_t i = 0; i < Modes.size(); i++)
    {
        pInArgs->pMonitorModes[i] = MonitorMode(Modes[i], IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR);
    }
    pOutArgs->PreferredMonitorModeIdx = 0;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddMonitorGetDefaultModes(IDDCX_MONITOR MonitorObject, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* pInArgs,
                                   IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* pOutArgs)
{
    // Only called for a monitor without an EDID, and the lab always sends one.
    vector<Mode> Modes;
    if (!g_Context || !g_Context->ModesForMonitor(MonitorObject, Modes))
    {
        Modes = {{1920, 1080, 60000}};
    }
    pOutArgs->DefaultMonitorModeBufferOutputCount = static_cast<UINT>(Modes.size());
    if (pInArgs->DefaultMonitorModeBufferInputCount >= Modes.size())
    {
        for (size_t i = 0; i < Modes.size(); i++)
        {
            pInArgs->pDefaultMonitorModes[i] = MonitorMode(Modes[i], IDDCX_MONITOR_MODE_ORIGIN_DRIVER);
        }
        pOutArgs->PreferredMonitorModeIdx = 0;
    }
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddMonitorQueryModes(IDDCX_MONITOR MonitorObject, const IDARG_IN_QUERYTARGETMODES* pInArgs, IDARG_OUT_QUERYTARGETMODES* pOutArgs)
{
    // The OS offers the intersection of monitor and target modes; offering
    // the same list here keeps it to exactly what the lab asked for.
    vector<Mode> Modes;
    if (!g_Context || !g_Context->ModesForMonitor(MonitorObject, Modes))
    {
        pOutArgs->TargetModeBufferOutputCount = 0;
        return STATUS_SUCCESS;
    }
    pOutArgs->TargetModeBufferOutputCount = static_cast<UINT>(Modes.size());
    if (pInArgs->TargetModeBufferInputCount >= Modes.size())
    {
        for (size_t i = 0; i < Modes.size(); i++)
        {
            pInArgs->pTargetModes[i] = TargetMode(Modes[i]);
        }
    }
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddMonitorAssignSwapChain(IDDCX_MONITOR MonitorObject, const IDARG_IN_SETSWAPCHAIN* pInArgs)
{
    auto* pMonitorContextWrapper = WdfObjectGet_IndirectMonitorContextWrapper(MonitorObject);
    pMonitorContextWrapper->pContext->AssignSwapChain(pInArgs->hSwapChain, pInArgs->RenderAdapterLuid, pInArgs->hNextSurfaceAvailable);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddMonitorUnassignSwapChain(IDDCX_MONITOR MonitorObject)
{
    auto* pMonitorContextWrapper = WdfObjectGet_IndirectMonitorContextWrapper(MonitorObject);
    pMonitorContextWrapper->pContext->UnassignSwapChain();
    return STATUS_SUCCESS;
}

#pragma endregion
