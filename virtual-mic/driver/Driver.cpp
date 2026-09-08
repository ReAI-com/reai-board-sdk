// ReAI-Vibe-Board virtual microphone driver.
//
// Input-only 16 kHz mono CoreAudio device. PCM samples arrive as UDP
// datagrams (native-endian SInt16) on 127.0.0.1:47160 from a user process
// running the reai-board-sdk `virtual-mic` feature, and are served to any
// app that reads from the microphone. When nobody feeds the port, the device
// presents silence.
//
// Based on the libASPL examples (MIT), Copyright (c) libASPL authors.

#include <aspl/Context.hpp>
#include <aspl/Device.hpp>
#include <aspl/Driver.hpp>
#include <aspl/IORequestHandler.hpp>
#include <aspl/Plugin.hpp>
#include <aspl/Stream.hpp>
#include <aspl/Tracer.hpp>

#include <CoreAudio/AudioServerPlugIn.h>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <atomic>
#include <cstring>
#include <memory>
#include <thread>
#include <vector>

namespace {

constexpr UInt32 kSampleRate = 16000;
constexpr int kUdpPort = 47160;

// ~1 s of audio, power of two.
constexpr size_t kRingCapacity = 16384;
// When the backlog grows past kHighWater, the consumer skips ahead to
// kLowWater: the feeding clock (board) and the IO clock (host) drift apart
// slowly, and this keeps latency bounded.
constexpr size_t kHighWater = 4800; // 300 ms
constexpr size_t kLowWater = 1600; // 100 ms

// Lock-free SPSC sample ring.
// Producer: UDP receiver thread (drops the new sample when full).
// Consumer: coreaudiod realtime IO thread (never blocks).
class SampleRing
{
public:
    void Push(SInt16 sample)
    {
        const size_t head = head_.load(std::memory_order_relaxed);
        const size_t tail = tail_.load(std::memory_order_acquire);
        const size_t next = (head + 1) % kRingCapacity;
        if (next == tail) {
            return; // full: nothing consumes, dropping new audio is fine
        }
        buffer_[head] = sample;
        head_.store(next, std::memory_order_release);
    }

    size_t Pop(SInt16* out, size_t count)
    {
        size_t tail = tail_.load(std::memory_order_relaxed);
        const size_t head = head_.load(std::memory_order_acquire);

        const size_t level = (head + kRingCapacity - tail) % kRingCapacity;
        if (level > kHighWater) {
            tail = (head + kRingCapacity * 2 - kLowWater) % kRingCapacity;
        }

        size_t popped = 0;
        while (popped < count && tail != head) {
            out[popped++] = buffer_[tail];
            tail = (tail + 1) % kRingCapacity;
        }
        tail_.store(tail, std::memory_order_release);
        return popped;
    }

private:
    std::vector<SInt16> buffer_ = std::vector<SInt16>(kRingCapacity, 0);
    std::atomic<size_t> head_ = 0;
    std::atomic<size_t> tail_ = 0;
};

// Blocks on recvfrom() and feeds the ring. Runs for the lifetime of the
// plugin process (coreaudiod).
class Receiver
{
public:
    Receiver(std::shared_ptr<SampleRing> ring, std::shared_ptr<aspl::Tracer> tracer)
        : ring_(std::move(ring))
        , tracer_(std::move(tracer))
        , socket_(::socket(AF_INET, SOCK_DGRAM, 0))
        , thread_([this] { Run(); })
    {
    }

    ~Receiver()
    {
        running_.store(false, std::memory_order_release);
        ::close(socket_); // unblocks recvfrom()
        if (thread_.joinable()) {
            thread_.join();
        }
    }

private:
    void Run()
    {
        sockaddr_in addr {};
        addr.sin_family = AF_INET;
        addr.sin_port = htons(kUdpPort);
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (::bind(socket_, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0) {
            tracer_->Message("ReAIVibeBoard: cannot bind UDP %d: errno=%d", kUdpPort, errno);
            return;
        }
        tracer_->Message("ReAIVibeBoard: listening on UDP 127.0.0.1:%d", kUdpPort);

        std::vector<char> datagram(8192);
        while (running_.load(std::memory_order_relaxed)) {
            const ssize_t received =
                ::recvfrom(socket_, datagram.data(), datagram.size(), 0, nullptr, nullptr);
            if (received <= 0) {
                break; // socket closed from the destructor
            }
            const SInt16* samples = reinterpret_cast<const SInt16*>(datagram.data());
            for (size_t i = 0; i < static_cast<size_t>(received) / sizeof(SInt16); ++i) {
                ring_->Push(samples[i]);
            }
        }
    }

    std::shared_ptr<SampleRing> ring_;
    std::shared_ptr<aspl::Tracer> tracer_;
    std::atomic<bool> running_ = true;
    int socket_ = -1;
    std::thread thread_;
};

// Serves microphone reads from the ring; silence on underrun.
class MicHandler : public aspl::IORequestHandler
{
public:
    explicit MicHandler(std::shared_ptr<SampleRing> ring)
        : ring_(std::move(ring))
    {
    }

    void OnReadClientInput(const std::shared_ptr<aspl::Client>& client,
        const std::shared_ptr<aspl::Stream>& stream,
        Float64 zeroTimestamp,
        Float64 timestamp,
        void* bytes,
        UInt32 bytesCount) override
    {
        SInt16* samples = static_cast<SInt16*>(bytes);
        const size_t count = bytesCount / sizeof(SInt16);
        const size_t popped = ring_->Pop(samples, count);
        for (size_t i = popped; i < count; ++i) {
            samples[i] = 0;
        }
    }

private:
    std::shared_ptr<SampleRing> ring_;
};

std::shared_ptr<aspl::Driver> CreateDriver()
{
    auto context = std::make_shared<aspl::Context>();

    aspl::DeviceParameters params;
    params.Name = "ReAI-Vibe-Board";
    params.Manufacturer = "ReAI";
    params.SampleRate = kSampleRate;
    params.ChannelCount = 1;
    params.DeviceUID = "com.reai.vibeboard.virtual-mic";
    params.ModelUID = "com.reai.vibeboard";
    params.SerialNumber = "VB-VIRT-0001";
    // CanBeDefault 必须为 true:系统设置的「输入」面板只列出可作默认的设备,
    // false 会让设备在面板里直接消失(实测)。macOS 不会因新设备出现而自动
    // 切换默认输入(仅当它是唯一输入设备时才会被选中),无需担心劫持。
    params.CanBeDefault = true;
    params.CanBeDefaultForSystemSounds = true;

    auto device = std::make_shared<aspl::Device>(context, params);
    device->AddStreamWithControlsAsync(aspl::Direction::Input);

    auto ring = std::make_shared<SampleRing>();
    device->SetIOHandler(std::make_shared<MicHandler>(ring));

    auto plugin = std::make_shared<aspl::Plugin>(context);
    plugin->AddDevice(device);
    auto driver = std::make_shared<aspl::Driver>(context, plugin);

    // Tied to static storage so the receiver thread outlives everything the
    // audio server may hold and stops at process exit.
    static std::unique_ptr<Receiver> receiver(
        new Receiver(ring, std::make_shared<aspl::Tracer>()));

    return driver;
}

} // namespace

extern "C" void* ReAIVibeBoardEntryPoint(CFAllocatorRef allocator, CFUUIDRef typeUUID)
{
    // The UUID of the plug-in type (AudioServerPlugIn).
    if (!CFEqual(typeUUID, kAudioServerPlugInTypeUUID)) {
        return nullptr;
    }

    static std::shared_ptr<aspl::Driver> driver = CreateDriver();

    return driver->GetReference();
}
