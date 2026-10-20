#include <cerrno>
#include <cstring>
#include <memory>
#include <poll.h>
#include <stdexcept>
#include <string>
#include <unistd.h>

#include <core/Configuration.h>
#include <core/Region.h>
#include <core/Timer.h>
#include <network/UnixSocket.h>
#include <rdr/FdOutStream.h>
#include <rfb/AccessRights.h>
#include <rfb/PixelBuffer.h>
#include <rfb/SDesktop.h>
#include <rfb/VNCServerST.h>

namespace {
thread_local std::string last_error;
constexpr int POLL_TIMEOUT_MS = 10;

class Desktop : public rfb::SDesktop {
public:
    rfb::VNCServer* server = nullptr;
    std::unique_ptr<rfb::ManagedPixelBuffer> pixels = std::make_unique<rfb::ManagedPixelBuffer>(
        rfb::PixelFormat(32, 24, false, true, 255, 255, 255, 0, 8, 16), 1, 1);

    void init(rfb::VNCServer* value) override { server = value; }
    void start() override { server->setPixelBuffer(pixels.get()); }
    void queryConnection(network::Socket* socket, const char*) override { server->approveConnection(socket, true, nullptr); }
    void terminate() override {}
};

struct Session {
    Desktop desktop;
    std::unique_ptr<network::UnixSocket> socket;
    rfb::VNCServerST server{"CodexApp2Web", &desktop};
};
}

extern "C" {
const char* vnc_error() noexcept { return last_error.c_str(); }

// The descriptor is borrowed; Rust retains ownership until this session is destroyed.
void* vnc_create(int fd) noexcept {
    try {
        core::Configuration::setParam("SecurityTypes", "None");
        core::Configuration::setParam("CompareFB", "1");
        core::Configuration::setParam("FrameRate", "30");

        auto session = std::make_unique<Session>();
        const int owned_fd = dup(fd);

        if (owned_fd < 0) throw std::runtime_error(std::strerror(errno));

        session->socket = std::make_unique<network::UnixSocket>(owned_fd);
        session->server.addSocket(session->socket.get(), false, rfb::AccessView | rfb::AccessNoQuery);

        return session.release();
    } catch (const std::exception& error) {
        last_error = error.what();
        return nullptr;
    }
}

bool vnc_frame(void* handle, const unsigned char* rgba, int width, int height) noexcept {
    try {
        auto& session = *static_cast<Session*>(handle);
        auto& pixels = session.desktop.pixels;

        if (pixels->width() != width || pixels->height() != height) {
            auto next = std::make_unique<rfb::ManagedPixelBuffer>(pixels->getPF(), width, height);

            next->imageRect(next->getRect(), rgba);
            session.server.setPixelBuffer(next.get());
            pixels = std::move(next);
        } else {
            pixels->imageRect(pixels->getRect(), rgba);
        }

        session.server.add_changed(core::Region(pixels->getRect()));

        return true;
    } catch (const std::exception& error) {
        last_error = error.what();
        return false;
    }
}

bool vnc_poll(void* handle) noexcept {
    try {
        auto& session = *static_cast<Session*>(handle);
        auto* socket = session.socket.get();
        pollfd event{socket->getFd(), POLLIN, 0};

        if (socket->outStream().hasBufferedData()) event.events |= POLLOUT;
        if (::poll(&event, 1, POLL_TIMEOUT_MS) < 0 && errno != EINTR) throw std::runtime_error(std::strerror(errno));
        if (event.revents & POLLIN) session.server.processSocketReadEvent(socket);
        if (event.revents & POLLOUT) session.server.processSocketWriteEvent(socket);
        if (event.revents & (POLLERR | POLLHUP | POLLNVAL)) throw std::runtime_error("VNC stream closed");
        if (socket->isShutdown()) throw std::runtime_error("VNC client disconnected");

        core::Timer::checkTimeouts();

        return true;
    } catch (const std::exception& error) {
        last_error = error.what();
        return false;
    }
}

void vnc_destroy(void* handle) noexcept { delete static_cast<Session*>(handle); }
}
