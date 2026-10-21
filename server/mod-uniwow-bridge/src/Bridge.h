// SPDX-License-Identifier: GPL-2.0-or-later
// The bridge (step 16.2): what the editor wrote into the world database loaded again by the server
// running, on the editor's demand, without starting it again. Its network thread only reads and
// queues the demands; they are made on the world thread, after the maps were updated, where the
// commands of the console are.

#ifndef MOD_UNIWOW_BRIDGE_H
#define MOD_UNIWOW_BRIDGE_H

#include "Define.h"

#include <atomic>
#include <condition_variable>
#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace UniwowBridge
{
    // What the editor asks the server to load again.
    enum class Action : uint8
    {
        // The quests, their givers and enders, their texts and points of interest.
        Quests = 1,
        // The waypoints of the path of this id.
        Path = 2,
        // The addons of every creature: their paths, mounts, auras.
        Addons = 3,
        // Every group of creatures.
        Formations = 4,
        // The creature of this guid read again from its row: taken away, spawned again where it
        // places it; taken away alone once its row is gone.
        Creature = 5,
        // The game object of this guid, as a creature.
        GameObject = 6,
    };

    struct Demand
    {
        Action action;
        uint64 value;
    };

    // What a demand made: whether it was, and what the server says of it.
    struct Outcome
    {
        bool done;
        std::string said;
    };

    // Demands queued by the network thread, made by the world thread.
    struct Request
    {
        std::vector<Demand> demands;
        std::vector<Outcome> outcomes;
        bool made = false;
    };

    class Bridge
    {
    public:
        static Bridge& Instance();

        // Reads its configuration and listens, when enabled; once the world is loaded.
        void Start();
        void Stop();

        // On the world thread, after the maps were updated: makes the demands queued.
        void Update();

        // On the network thread: queues `request`, then waits until it is made, or for `seconds`.
        bool Make(std::shared_ptr<Request> const& request, uint32 seconds);

    private:
        void Listen();
        Outcome MakeOne(Demand const& demand);

        std::atomic<bool> _running{ false };
        std::thread _thread;
        uint16 _port = 8092;
        std::string _token;

        std::mutex _lock;
        std::condition_variable _made;
        std::deque<std::shared_ptr<Request>> _queue;
    };
}

#endif
