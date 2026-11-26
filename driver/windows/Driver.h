// Display Lab indirect display driver.
//
// Based on Microsoft's IddSampleDriver (Windows-driver-samples,
// video/IndirectDisplay), used under the Microsoft Public License; see
// LICENSE-MS-PL in this folder. Changes: monitors are plugged and unplugged
// one at a time over a named pipe, each with the EDID and modes the lab
// sends, instead of a fixed set at start-up.

#pragma once

#define NOMINMAX
#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>

#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <avrt.h>
#include <wrl.h>

#include <memory>
#include <string>
#include <vector>

namespace Microsoft
{
    namespace WRL
    {
        namespace Wrappers
        {
            // Adds a wrapper for thread handles to the existing set of WRL handle wrapper classes
            typedef HandleT<HandleTraits::HANDLENullTraits> Thread;
        }
    }
}

namespace DisplayLab
{
    // One mode, with the refresh rate in millihertz so 59.94 Hz is exact.
    struct Mode
    {
        DWORD Width;
        DWORD Height;
        DWORD RefreshMilliHz;
    };

    // How many monitors the lab can plug at once.
    static constexpr UINT MaxSlots = 16;

    // A connector the lab can plug a monitor into.
    struct Slot
    {
        bool Used = false;
        IDDCX_MONITOR Monitor = nullptr;
        std::vector<BYTE> Edid;
        std::vector<Mode> Modes;
    };

    struct Direct3DDevice
    {
        Direct3DDevice(LUID AdapterLuid);
        HRESULT Init();

        LUID AdapterLuid;
        Microsoft::WRL::ComPtr<IDXGIFactory5> DxgiFactory;
        Microsoft::WRL::ComPtr<IDXGIAdapter1> Adapter;
        Microsoft::WRL::ComPtr<ID3D11Device> Device;
        Microsoft::WRL::ComPtr<ID3D11DeviceContext> DeviceContext;
    };

    // Consumes the buffers of one monitor's swap chain. Windows composes the
    // desktop into them; the lab captures the screen through the desktop, so
    // the driver only has to keep them moving.
    class SwapChainProcessor
    {
    public:
        SwapChainProcessor(IDDCX_SWAPCHAIN hSwapChain, std::shared_ptr<Direct3DDevice> Device, HANDLE NewFrameEvent);
        ~SwapChainProcessor();

    private:
        static DWORD CALLBACK RunThread(LPVOID Argument);
        void Run();
        void RunCore();

        IDDCX_SWAPCHAIN m_hSwapChain;
        std::shared_ptr<Direct3DDevice> m_Device;
        HANDLE m_hAvailableBufferEvent;
        Microsoft::WRL::Wrappers::Thread m_hThread;
        Microsoft::WRL::Wrappers::Event m_hTerminateEvent;
    };

    class IndirectDeviceContext
    {
    public:
        IndirectDeviceContext(_In_ WDFDEVICE WdfDevice);
        virtual ~IndirectDeviceContext();

        void InitAdapter();

        // Pipe commands. Each returns the reply line.
        std::string Plug(UINT Slot, std::vector<BYTE> Edid, std::vector<Mode> Modes);
        std::string Unplug(UINT Slot);
        std::string List();

        // For the IddCx callbacks: the modes of the monitor with this EDID or
        // this handle. False when the lab plugged no such monitor.
        bool ModesForEdid(const BYTE* Edid, UINT Size, std::vector<Mode>& Out);
        bool ModesForMonitor(IDDCX_MONITOR Monitor, std::vector<Mode>& Out);

        IDDCX_ADAPTER m_Adapter;

    protected:
        WDFDEVICE m_WdfDevice;
        Slot m_Slots[MaxSlots];
        SRWLOCK m_Lock = SRWLOCK_INIT;
    };

    class IndirectMonitorContext
    {
    public:
        IndirectMonitorContext(_In_ IDDCX_MONITOR Monitor);
        virtual ~IndirectMonitorContext();

        void AssignSwapChain(IDDCX_SWAPCHAIN SwapChain, LUID RenderAdapter, HANDLE NewFrameEvent);
        void UnassignSwapChain();

    private:
        IDDCX_MONITOR m_Monitor;
        std::unique_ptr<SwapChainProcessor> m_ProcessingThread;
    };

    // The control pipe: started once the adapter is ready.
    void StartPipeServer(IndirectDeviceContext* Context);
}
