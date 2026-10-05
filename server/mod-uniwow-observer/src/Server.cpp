// SPDX-License-Identifier: GPL-2.0-or-later

#include "Server.h"

#include "GitRevision.h"
#include "GridDefines.h"
#include "Log.h"

#include <boost/asio/post.hpp>
#include <boost/asio/read.hpp>
#include <boost/asio/write.hpp>

#include <cmath>
#include <cstring>
#include <exception>

namespace UniwowObserver
{
    namespace
    {
        using boost::asio::ip::tcp;

        constexpr uint16 VERSION = 1;
        constexpr uint32 CAPABILITY_READING = 0x1;
        // The longest message from the editor, and from the observer.
        constexpr uint32 MAX_IN = 1024;
        constexpr size_t MAX_OUT = 16u << 20;
        // What may wait to be written to an editor before it is closed as too slow to read.
        constexpr size_t MAX_QUEUED = 32u << 20;
        // How long a subscription waits for an update of its map before it is told none came.
        constexpr auto NOT_FOUND = std::chrono::seconds(2);

        enum : uint8
        {
            HELLO = 1,
            SUBSCRIBE = 2,
            UNSUBSCRIBE = 3,
            HEARTBEAT = 4,
            WELCOME = 101,
            REFUSED = 102,
            STATUS = 103,
            SNAPSHOT = 104,
            CHANGES = 105,
        };

        // Little-endian bytes: a message, its length filled by `Finish`, or the bytes of an entity.
        class Writer
        {
        public:
            Writer() = default;
            explicit Writer(uint8 kind) : _message(true)
            {
                _bytes.resize(4);
                U8(kind);
            }

            void U8(uint8 value) { _bytes.push_back(value); }
            void U16(uint16 value) { Little(value, 2); }
            void U32(uint32 value) { Little(value, 4); }
            void U64(uint64 value) { Little(value, 8); }
            void I16(int16 value) { U16(uint16(value)); }

            void F32(float value)
            {
                uint32 bits = 0;
                std::memcpy(&bits, &value, sizeof(bits));
                U32(bits);
            }

            void String(std::string const& value)
            {
                size_t const length = std::min<size_t>(value.size(), 0xFFFF);
                U16(uint16(length));
                _bytes.insert(_bytes.end(), value.begin(), value.begin() + length);
            }

            void Bytes(std::vector<uint8> const& bytes) { _bytes.insert(_bytes.end(), bytes.begin(), bytes.end()); }

            std::vector<uint8> Finish()
            {
                if (_message)
                {
                    uint32 const length = uint32(_bytes.size() - 4);
                    for (int index = 0; index < 4; ++index)
                        _bytes[index] = uint8(length >> (8 * index));
                }
                return std::move(_bytes);
            }

        private:
            void Little(uint64 value, int count)
            {
                for (int index = 0; index < count; ++index)
                    _bytes.push_back(uint8(value >> (8 * index)));
            }

            bool _message = false;
            std::vector<uint8> _bytes;
        };

        // The body of a message from the editor, read within its bounds.
        class Reader
        {
        public:
            explicit Reader(std::vector<uint8> const& bytes) : _bytes(bytes) { }

            bool U16(uint16& value) { return Little(value, 2); }
            bool U32(uint32& value) { return Little(value, 4); }

            bool F32(float& value)
            {
                uint32 bits = 0;
                if (!U32(bits))
                    return false;
                std::memcpy(&value, &bits, sizeof(value));
                return true;
            }

            bool String(std::string& value, size_t longest)
            {
                uint16 length = 0;
                if (!U16(length) || length > longest || _bytes.size() - _at < length)
                    return false;
                value.assign(_bytes.begin() + _at, _bytes.begin() + _at + length);
                _at += length;
                return true;
            }

            bool Done() const { return _at == _bytes.size(); }

        private:
            template<class T>
            bool Little(T& value, size_t count)
            {
                if (_bytes.size() - _at < count)
                    return false;
                uint64 result = 0;
                for (size_t index = 0; index < count; ++index)
                    result |= uint64(_bytes[_at + index]) << (8 * index);
                value = T(result);
                _at += count;
                return true;
            }

            std::vector<uint8> const& _bytes;
            size_t _at = 0;
        };

        // The bytes of an entity; `compared`, those it is compared by: while it follows a spline, its
        // position, orientation and time along it are left out, which the spline gives.
        std::vector<uint8> Encode(Record const& record, bool compared = false)
        {
            bool const moving = compared && !record.path.empty();
            Writer writer;
            writer.U64(record.guid);
            writer.U8(uint8(record.kind));
            writer.U32(record.entry);
            writer.U32(record.spawn);
            writer.U8(record.flags);
            writer.U32(record.pool);
            writer.I16(record.event);
            writer.U32(record.phase);
            writer.U32(record.display);
            writer.F32(moving ? 0.0f : record.x);
            writer.F32(moving ? 0.0f : record.y);
            writer.F32(moving ? 0.0f : record.z);
            writer.F32(moving ? 0.0f : record.orientation);
            writer.F32(record.scale);
            if (record.kind == Kind::GameObject)
            {
                for (float value : record.rotation)
                    writer.F32(value);
                writer.U8(record.state);
            }
            writer.U8(uint8(record.path.size()));
            if (!record.path.empty())
            {
                writer.U32(record.spline);
                writer.U8(record.splineFlags);
                writer.U32(moving ? 0 : record.elapsed);
                for (PathPoint const& point : record.path)
                {
                    writer.F32(point.x);
                    writer.F32(point.y);
                    writer.F32(point.z);
                    writer.U32(point.time);
                }
            }
            writer.String(record.name);
            return writer.Finish();
        }

        uint32 LengthOf(std::array<uint8, 4> const& header)
        {
            return uint32(header[0]) | uint32(header[1]) << 8 | uint32(header[2]) << 16 | uint32(header[3]) << 24;
        }
    }

    Connection::Connection(Server& server, tcp::socket socket, uint64 id)
        : _server(server), _socket(std::move(socket)), _id(id), _heard(Clock::now())
    {
    }

    void Connection::Start()
    {
        ReadHeader();
    }

    void Connection::ReadHeader()
    {
        auto self = shared_from_this();
        boost::asio::async_read(_socket, boost::asio::buffer(_header), [this, self](boost::system::error_code error, size_t)
        {
            if (_closed)
                return;
            if (error)
            {
                Close("disconnected");
                return;
            }
            uint32 const length = LengthOf(_header);
            if (!length || length > MAX_IN)
            {
                Close("a message of a wrong length");
                return;
            }
            ReadBody(length);
        });
    }

    void Connection::ReadBody(uint32 length)
    {
        _body.resize(length);
        auto self = shared_from_this();
        boost::asio::async_read(_socket, boost::asio::buffer(_body), [this, self](boost::system::error_code error, size_t)
        {
            if (_closed)
                return;
            if (error)
            {
                Close("disconnected in the middle of a message");
                return;
            }
            _heard = Clock::now();
            std::vector<uint8> const body(_body.begin() + 1, _body.end());
            if (!Handle(_body[0], body))
            {
                Close("a message malformed");
                return;
            }
            if (!_closed && !_closing)
                ReadHeader();
        });
    }

    bool Connection::Handle(uint8 kind, std::vector<uint8> const& body)
    {
        Settings const& settings = _server.GetSettings();
        Reader reader(body);
        if (!_welcomed)
        {
            uint16 version = 0;
            std::string token;
            if (kind != HELLO || !reader.U16(version) || !reader.String(token, 256) || !reader.Done())
                return false;
            if (version != VERSION)
            {
                Refuse("version " + std::to_string(version) + " of the protocol is not served, 1 is");
                return true;
            }
            if (settings.token.empty() || token != settings.token)
            {
                Refuse("the token is not that of UniwowObserver.Token");
                return true;
            }
            _welcomed = true;
            Writer writer(WELCOME);
            writer.U16(VERSION);
            writer.U32(CAPABILITY_READING);
            writer.String(GitRevision::GetFullVersion());
            writer.String(GitRevision::GetHash());
            writer.F32(settings.maxRadius);
            writer.U32(settings.maxEntities);
            writer.U16(uint16(settings.rate));
            writer.U16(uint16(settings.heartbeat));
            Send(writer.Finish());
            return true;
        }
        switch (kind)
        {
            case SUBSCRIBE:
            {
                Zone zone;
                if (!reader.U32(zone.map) || !reader.U32(zone.instance) || !reader.F32(zone.x) || !reader.F32(zone.y)
                    || !reader.F32(zone.z) || !reader.F32(zone.radius) || !reader.Done())
                    return false;
                if (!Acore::IsValidMapCoord(zone.x, zone.y) || !std::isfinite(zone.z) || !std::isfinite(zone.radius) || zone.radius <= 0.0f)
                    return false;
                zone.radius = std::min(zone.radius, settings.maxRadius);
                Subscribe(zone);
                return true;
            }
            case UNSUBSCRIBE:
                if (!reader.Done())
                    return false;
                Unsubscribe();
                return true;
            case HEARTBEAT:
                return reader.Done();
            default:
                return false;
        }
    }

    void Connection::Subscribe(Zone const& zone)
    {
        bool const fresh = !_subscription;
        if (fresh)
            _subscription = std::make_shared<Subscription>();
        bool moved = false;
        {
            std::lock_guard<std::mutex> guard(_subscription->lock);
            moved = !fresh && _subscription->zone.map == zone.map && _subscription->zone.instance == zone.instance;
            _subscription->zone = zone;
            ++_subscription->version;
            if (!moved)
            {
                ++_subscription->generation;
                _subscription->state = State::Waiting;
                _subscription->reading.reset();
                _subscription->kept.reset();
            }
        }
        if (fresh)
            _server.PublishSubscriptions();
        // Moved on the same map: what was sent stays, the next readings send what changed.
        if (moved)
            return;
        _subscribed = Clock::now();
        _status = State::Waiting;
        _snapshot = false;
        _sequence = 0;
        _sent.clear();
        SendStatus(State::Waiting, zone);
    }

    void Connection::Unsubscribe()
    {
        if (!_subscription)
            return;
        {
            std::lock_guard<std::mutex> guard(_subscription->lock);
            _subscription->closed = true;
            _subscription->reading.reset();
            _subscription->kept.reset();
        }
        _subscription.reset();
        _sent.clear();
        _server.PublishSubscriptions();
    }

    void Connection::SendStatus(State state, Zone const& zone)
    {
        Writer writer(STATUS);
        writer.U8(uint8(state));
        writer.U32(zone.map);
        writer.U32(zone.instance);
        writer.F32(zone.radius);
        Send(writer.Finish());
    }

    void Connection::Tick(Clock::time_point now)
    {
        if (_closed)
            return;
        if (now - _heard > std::chrono::seconds(_server.GetSettings().heartbeat))
        {
            Close("silent");
            return;
        }
        if (!_welcomed || _closing || !_subscription)
            return;

        State state;
        Zone zone;
        std::shared_ptr<Reading const> reading;
        {
            std::lock_guard<std::mutex> guard(_subscription->lock);
            state = _subscription->state;
            zone = _subscription->zone;
            reading = _subscription->reading;
        }
        if (state == State::Waiting)
        {
            if (_status == State::Waiting && now - _subscribed > NOT_FOUND)
            {
                _status = State::NotFound;
                SendStatus(State::NotFound, zone);
            }
            return;
        }
        if (!reading || reading->sequence == _sequence)
            return;
        _sequence = reading->sequence;

        std::unordered_map<uint64, std::vector<uint8>> sent;
        sent.reserve(reading->records.size());
        if (!_snapshot)
        {
            Writer writer(SNAPSHOT);
            writer.U32(zone.map);
            writer.U32(zone.instance);
            writer.U64(_sequence);
            writer.U32(uint32(reading->records.size()));
            for (Record const& record : reading->records)
            {
                writer.Bytes(Encode(record));
                sent[record.guid] = Encode(record, true);
            }
            _sent = std::move(sent);
            _snapshot = true;
            Send(writer.Finish());
            _status = State::Active;
            SendStatus(State::Active, zone);
            return;
        }

        std::vector<std::vector<uint8>> changed;
        for (Record const& record : reading->records)
        {
            std::vector<uint8> compared = Encode(record, true);
            auto const before = _sent.find(record.guid);
            if (before == _sent.end() || before->second != compared)
                changed.push_back(Encode(record));
            sent[record.guid] = std::move(compared);
        }
        std::vector<uint64> left;
        for (auto const& [guid, bytes] : _sent)
            if (!sent.count(guid))
                left.push_back(guid);
        _sent = std::move(sent);
        if (changed.empty() && left.empty())
            return;

        Writer writer(CHANGES);
        writer.U32(zone.map);
        writer.U32(zone.instance);
        writer.U64(_sequence);
        writer.U32(uint32(changed.size()));
        for (std::vector<uint8> const& bytes : changed)
            writer.Bytes(bytes);
        writer.U32(uint32(left.size()));
        for (uint64 guid : left)
            writer.U64(guid);
        Send(writer.Finish());
    }

    void Connection::LogStatistics(double seconds)
    {
        if (_closed || !_subscription)
            return;
        Timing reads;
        Timing keeps;
        Zone zone;
        size_t entities = 0;
        {
            std::lock_guard<std::mutex> guard(_subscription->lock);
            reads = _subscription->reads;
            keeps = _subscription->keeps;
            _subscription->reads = {};
            _subscription->keeps = {};
            zone = _subscription->zone;
            if (_subscription->reading)
                entities = _subscription->reading->records.size();
        }
        auto const mean = [](Timing const& timing) { return timing.count ? timing.sum / double(timing.count) : 0.0; };
        LOG_INFO("module", "uniwow-observer: connection {}, map {} instance {}: {} entities; reading {:.3f} ms on average, {:.3f} at most, {} times; keeping {:.3f} ms on average, {:.3f} at most, {} times; {:.0f} bytes a second",
            _id, zone.map, zone.instance, entities, mean(reads), reads.longest, reads.count, mean(keeps), keeps.longest, keeps.count,
            seconds > 0.0 ? double(_bytes) / seconds : 0.0);
        _bytes = 0;
    }

    void Connection::Send(std::vector<uint8> message)
    {
        if (_closed)
            return;
        if (message.size() > MAX_OUT)
        {
            Close("a message too long to send");
            return;
        }
        _queued += message.size();
        _bytes += message.size();
        if (_queued > MAX_QUEUED)
        {
            Close("too slow to read what is sent");
            return;
        }
        _queue.push_back(std::move(message));
        if (!_writing)
            Write();
    }

    void Connection::Write()
    {
        _writing = true;
        auto self = shared_from_this();
        boost::asio::async_write(_socket, boost::asio::buffer(_queue.front()), [this, self](boost::system::error_code error, size_t)
        {
            _writing = false;
            if (_closed)
                return;
            if (error)
            {
                Close("disconnected");
                return;
            }
            _queued -= _queue.front().size();
            _queue.pop_front();
            if (!_queue.empty())
                Write();
            else if (_closing)
                Close("refused");
        });
    }

    void Connection::Refuse(std::string const& reason)
    {
        LOG_INFO("module", "uniwow-observer: connection {} refused: {}", _id, reason);
        Writer writer(REFUSED);
        writer.String(reason);
        _closing = true;
        Send(writer.Finish());
    }

    void Connection::Close(char const* reason)
    {
        if (_closed)
            return;
        auto self = shared_from_this();
        _closed = true;
        boost::system::error_code ignored;
        _socket.shutdown(tcp::socket::shutdown_both, ignored);
        _socket.close(ignored);
        if (_subscription)
        {
            std::lock_guard<std::mutex> guard(_subscription->lock);
            _subscription->closed = true;
            _subscription->reading.reset();
            _subscription->kept.reset();
        }
        LOG_INFO("module", "uniwow-observer: connection {} closed: {}", _id, reason);
        _server.Remove(this);
    }

    Server::Server(Settings const& settings) : _settings(settings), _acceptor(_io), _timer(_io), _retry(_io)
    {
    }

    Server::~Server()
    {
        Stop();
    }

    bool Server::Start()
    {
        boost::system::error_code error;
        tcp::endpoint const endpoint(boost::asio::ip::make_address_v4("127.0.0.1"), _settings.port);
        _acceptor.open(endpoint.protocol(), error);
        if (!error)
            _acceptor.bind(endpoint, error);
        if (!error)
            _acceptor.listen(boost::asio::socket_base::max_listen_connections, error);
        if (error)
        {
            LOG_ERROR("module", "uniwow-observer: port {} not opened: {}", _settings.port, error.message());
            return false;
        }
        _statistics = Clock::now();
        Accept();
        Tick();
        _thread = std::thread([this] { Run(); });
        return true;
    }

    void Server::Stop()
    {
        if (!_thread.joinable())
            return;
        boost::asio::post(_io, [this]
        {
            boost::system::error_code ignored;
            _acceptor.close(ignored);
            _timer.cancel();
            _retry.cancel();
            for (std::shared_ptr<Connection> const& connection : std::vector<std::shared_ptr<Connection>>(_connections))
                connection->Close("the server stops");
        });
        // What was posted closes everything; then the thread stops whatever is left.
        std::this_thread::sleep_for(std::chrono::milliseconds(100));
        _io.stop();
        _thread.join();
    }

    void Server::Run()
    {
        for (;;)
        {
            try
            {
                _io.run();
                return;
            }
            catch (std::exception const& error)
            {
                LOG_ERROR("module", "uniwow-observer: {}", error.what());
            }
            catch (...)
            {
                LOG_ERROR("module", "uniwow-observer: an unknown error in the network thread");
            }
        }
    }

    void Server::Accept()
    {
        _acceptor.async_accept([this](boost::system::error_code error, tcp::socket socket)
        {
            if (!_acceptor.is_open())
                return;
            // An error that lasts, such as no handle left, is not retried at once.
            if (error)
            {
                _retry.expires_after(std::chrono::milliseconds(100));
                _retry.async_wait([this](boost::system::error_code cancelled)
                {
                    if (!cancelled)
                        Accept();
                });
                return;
            }
            auto connection = std::make_shared<Connection>(*this, std::move(socket), _next++);
            if (_connections.size() >= _settings.maxConnections)
                connection->Refuse("too many connections, " + std::to_string(_settings.maxConnections) + " at most");
            else
            {
                _connections.push_back(connection);
                connection->Start();
            }
            Accept();
        });
    }

    void Server::Tick()
    {
        _timer.expires_after(std::chrono::milliseconds(1000 / _settings.rate));
        _timer.async_wait([this](boost::system::error_code error)
        {
            if (error)
                return;
            Clock::time_point const now = Clock::now();
            std::vector<std::shared_ptr<Connection>> const connections(_connections);
            for (std::shared_ptr<Connection> const& connection : connections)
                connection->Tick(now);
            if (_settings.statsInterval && now - _statistics >= std::chrono::seconds(_settings.statsInterval))
            {
                double const seconds = std::chrono::duration<double>(now - _statistics).count();
                for (std::shared_ptr<Connection> const& connection : connections)
                    connection->LogStatistics(seconds);
                _statistics = now;
            }
            Tick();
        });
    }

    void Server::Remove(Connection* connection)
    {
        auto const found = std::find_if(_connections.begin(), _connections.end(),
            [connection](std::shared_ptr<Connection> const& other) { return other.get() == connection; });
        if (found != _connections.end())
            _connections.erase(found);
        PublishSubscriptions();
    }

    void Server::PublishSubscriptions()
    {
        Subscriptions subscriptions;
        for (std::shared_ptr<Connection> const& connection : _connections)
            if (std::shared_ptr<Subscription> const& subscription = connection->GetSubscription())
                subscriptions.push_back(subscription);
        Observer::Instance().Publish(std::move(subscriptions));
    }
}
