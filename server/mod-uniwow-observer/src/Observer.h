// SPDX-License-Identifier: GPL-2.0-or-later
// The observer of UniWoW: the zones subscribed to, read on the threads of their maps, shared with
// the network thread through one lock per subscription.

#ifndef MOD_UNIWOW_OBSERVER_OBSERVER_H
#define MOD_UNIWOW_OBSERVER_OBSERVER_H

#include "Define.h"
#include "ObjectGuid.h"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

class Map;

namespace UniwowObserver
{
    using Clock = std::chrono::steady_clock;

    // The settings, read once when the worldserver starts.
    struct Settings
    {
        bool enable = true;
        uint16 port = 8087;
        std::string token;
        uint32 rate = 10;
        float maxRadius = 533.0f;
        uint32 maxEntities = 2000;
        uint32 maxConnections = 4;
        uint32 heartbeat = 10;
        bool keepAlive = true;
        uint32 statsInterval = 0;
    };

    enum class Kind : uint8
    {
        Creature = 1,
        GameObject = 2,
        Player = 3,
    };

    enum Flag : uint8
    {
        FLAG_TEMPORARY = 0x01,
        FLAG_DEAD = 0x02,
        FLAG_WALKING = 0x04,
        FLAG_FLYING = 0x08,
        FLAG_MOVING = 0x10,
        FLAG_GAME_MASTER = 0x20,
    };

    // The points of a spline sent at most.
    constexpr size_t PATH_POINTS = 32;
    // The bytes of a name sent at most.
    constexpr size_t NAME_BYTES = 64;

    // An entity as read on the thread of its map: plain values, no pointer into the game.
    struct Record
    {
        uint64 guid = 0;
        Kind kind = Kind::Creature;
        uint32 entry = 0;
        uint32 spawn = 0;
        uint8 flags = 0;
        uint32 pool = 0;
        int16 event = 0;
        uint32 phase = 0;
        uint32 display = 0;
        float x = 0.0f, y = 0.0f, z = 0.0f, orientation = 0.0f, scale = 1.0f;
        std::array<float, 4> rotation = { 0.0f, 0.0f, 0.0f, 1.0f };
        uint8 state = 0;
        uint32 duration = 0;
        uint32 elapsed = 0;
        std::vector<std::array<float, 3>> path;
        std::string name;
        // The square of its distance to the centre, on the ground: not sent.
        float distance = 0.0f;
    };

    struct Zone
    {
        uint32 map = 0;
        uint32 instance = 0;
        float x = 0.0f, y = 0.0f, z = 0.0f, radius = 0.0f;
    };

    enum class State : uint8
    {
        Waiting = 0,
        Active = 1,
        NotFound = 2,
    };

    // The mean and the longest of a time, in milliseconds.
    struct Timing
    {
        double sum = 0.0;
        double longest = 0.0;
        uint64 count = 0;

        void Add(double ms)
        {
            sum += ms;
            longest = std::max(longest, ms);
            ++count;
        }
    };

    struct Reading
    {
        std::vector<Record> records;
        uint64 sequence = 0;
    };

    // A subscription: its zone set by the network thread, its readings left by the thread of its
    // map. Every field is under `lock`, which both hold briefly and never while waiting.
    struct Subscription
    {
        std::mutex lock;
        Zone zone;
        // Increased at each change of the zone.
        uint64 version = 0;
        bool closed = false;
        State state = State::Waiting;
        std::shared_ptr<Reading const> reading;
        uint64 sequence = 0;
        // The version of the zone the last reading was of, and when it was made.
        uint64 readVersion = 0;
        bool read = false;
        Clock::time_point lastRead;
        // The version of the zone whose grids were loaded.
        uint64 loadedVersion = 0;
        bool loaded = false;
        // What the last reading found to keep updated.
        std::shared_ptr<std::vector<ObjectGuid> const> kept;
        Timing reads;
        Timing keeps;
    };

    using Subscriptions = std::vector<std::shared_ptr<Subscription>>;

    class Server;

    class Observer
    {
    public:
        static Observer& Instance();

        // At the start of the worldserver, on its thread: reads the settings, the game events of
        // the spawns, and opens the port.
        void Start();
        // At its shutdown, before the maps are unloaded.
        void Stop();
        // At the end of the update of `map`, on its thread.
        void Update(Map* map);

        Settings const& GetSettings() const { return _settings; }
        // The subscriptions the threads of the maps read, replaced by the network thread.
        void Publish(Subscriptions subscriptions);

    private:
        void Update(Map* map, Subscription& subscription);
        void Read(Map* map, Zone const& zone, Reading& reading, std::vector<ObjectGuid>& kept) const;
        int16 EventOf(Kind kind, uint32 spawn) const;

        friend struct Visitor;

        Settings _settings;
        Clock::duration _period{};
        std::atomic<bool> _running{ false };
        std::atomic<std::shared_ptr<Subscriptions const>> _subscriptions;
        std::unordered_map<uint32, int16> _creatureEvents;
        std::unordered_map<uint32, int16> _gameObjectEvents;
        std::unique_ptr<Server> _server;
    };
}

#endif
