// SPDX-License-Identifier: GPL-2.0-or-later
// The network of the observer: one thread of its own, listening on 127.0.0.1, never touching an
// object of the game; it reads what the threads of the maps leave in the subscriptions.

#ifndef MOD_UNIWOW_OBSERVER_SERVER_H
#define MOD_UNIWOW_OBSERVER_SERVER_H

#include "Observer.h"

#include <boost/asio/io_context.hpp>
#include <boost/asio/ip/tcp.hpp>
#include <boost/asio/steady_timer.hpp>

#include <deque>
#include <thread>

namespace UniwowObserver
{
    class Server;

    class Connection : public std::enable_shared_from_this<Connection>
    {
    public:
        Connection(Server& server, boost::asio::ip::tcp::socket socket, uint64 id);

        void Start();
        // Sends what was read since the last tick, closes it when silent too long.
        void Tick(Clock::time_point now);
        // Writes the statistics of its subscription to the log, then starts them again.
        void LogStatistics(double seconds);
        void Close(char const* reason);
        // Sends REFUSED, then closes once it is written.
        void Refuse(std::string const& reason);
        std::shared_ptr<Subscription> const& GetSubscription() const { return _subscription; }

    private:
        void ReadHeader();
        void ReadBody(uint32 length);
        bool Handle(uint8 kind, std::vector<uint8> const& body);
        void Subscribe(Zone const& zone);
        void Unsubscribe();
        void Send(std::vector<uint8> message);
        void Write();
        void SendStatus(State state, Zone const& zone);

        Server& _server;
        boost::asio::ip::tcp::socket _socket;
        uint64 _id;
        bool _closed = false;
        bool _welcomed = false;
        bool _closing = false;
        std::array<uint8, 4> _header{};
        std::vector<uint8> _body;
        std::deque<std::vector<uint8>> _queue;
        size_t _queued = 0;
        bool _writing = false;
        Clock::time_point _heard;
        std::shared_ptr<Subscription> _subscription;
        Clock::time_point _subscribed;
        State _status = State::Waiting;
        bool _snapshot = false;
        uint64 _sequence = 0;
        // The bytes of each entity last sent, by GUID.
        std::unordered_map<uint64, std::vector<uint8>> _sent;
        uint64 _bytes = 0;
    };

    class Server
    {
    public:
        explicit Server(Settings const& settings);
        ~Server();

        // Opens the port and starts the thread; false, said in the log, when the port cannot be
        // opened.
        bool Start();
        void Stop();

        Settings const& GetSettings() const { return _settings; }
        // A connection closed: forgotten, and the subscriptions published again.
        void Remove(Connection* connection);
        void PublishSubscriptions();

    private:
        void Accept();
        void Tick();
        void Run();

        Settings _settings;
        boost::asio::io_context _io;
        boost::asio::ip::tcp::acceptor _acceptor;
        boost::asio::steady_timer _timer;
        std::thread _thread;
        std::vector<std::shared_ptr<Connection>> _connections;
        uint64 _next = 1;
        Clock::time_point _statistics;
    };
}

#endif
