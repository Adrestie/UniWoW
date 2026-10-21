// SPDX-License-Identifier: GPL-2.0-or-later

#include "Bridge.h"

#include "Config.h"
#include "Creature.h"
#include "CreatureGroups.h"
#include "DatabaseEnv.h"
#include "GameObject.h"
#include "GitRevision.h"
#include "Log.h"
#include "Map.h"
#include "MapMgr.h"
#include "ObjectMgr.h"
#include "Player.h"
#include "PoolMgr.h"
#include "StringFormat.h"
#include "Transport.h"
#include "WaypointMgr.h"
#include "WorldSession.h"
#include "WorldSessionMgr.h"

#include <boost/asio/io_context.hpp>
#include <boost/asio/ip/tcp.hpp>
#include <boost/asio/read.hpp>
#include <boost/asio/write.hpp>

#include <array>
#include <chrono>
#include <cstring>
#include <exception>

namespace UniwowBridge
{
    namespace
    {
        using boost::asio::ip::tcp;

        constexpr uint16 VERSION = 1;
        // The longest message from the editor: 256 demands.
        constexpr uint32 MAX_IN = 4096;
        constexpr uint16 MAX_DEMANDS = 256;
        // How long a connection may say nothing, and a request wait for the world.
        constexpr auto SILENCE = std::chrono::seconds(10);
        constexpr uint32 WAIT_SECONDS = 30;

        enum : uint8
        {
            HELLO = 1,
            RELOAD = 2,
            WELCOME = 101,
            REFUSED = 102,
            DONE = 103,
        };

        // A message, little-endian, its length filled by `Finish`.
        class Writer
        {
        public:
            explicit Writer(uint8 kind)
            {
                _bytes.resize(4);
                U8(kind);
            }

            void U8(uint8 value) { _bytes.push_back(value); }
            void U16(uint16 value) { Little(value, 2); }
            void U32(uint32 value) { Little(value, 4); }

            void String(std::string const& value)
            {
                size_t const length = std::min<size_t>(value.size(), 0xFFFF);
                U16(uint16(length));
                _bytes.insert(_bytes.end(), value.begin(), value.begin() + length);
            }

            std::vector<uint8> Finish()
            {
                uint32 const length = uint32(_bytes.size() - 4);
                for (int index = 0; index < 4; ++index)
                    _bytes[index] = uint8(length >> (8 * index));
                return std::move(_bytes);
            }

        private:
            void Little(uint64 value, int count)
            {
                for (int index = 0; index < count; ++index)
                    _bytes.push_back(uint8(value >> (8 * index)));
            }

            std::vector<uint8> _bytes;
        };

        // The body of a message, read within its bounds.
        class Reader
        {
        public:
            explicit Reader(std::vector<uint8> const& bytes) : _bytes(bytes) { }

            bool U8(uint8& value) { return Little(value, 1); }
            bool U16(uint16& value) { return Little(value, 2); }
            bool U32(uint32& value) { return Little(value, 4); }
            bool U64(uint64& value) { return Little(value, 8); }

            bool String(std::string& value, size_t most)
            {
                uint16 length = 0;
                if (!U16(length) || length > most || _at + length > _bytes.size())
                    return false;
                value.assign(reinterpret_cast<char const*>(_bytes.data() + _at), length);
                _at += length;
                return true;
            }

            bool Done() const { return _at == _bytes.size(); }

        private:
            template <typename T>
            bool Little(T& value, size_t count)
            {
                if (_at + count > _bytes.size())
                    return false;
                uint64 read = 0;
                for (size_t index = 0; index < count; ++index)
                    read |= uint64(_bytes[_at + index]) << (8 * index);
                _at += count;
                value = T(read);
                return true;
            }

            std::vector<uint8> const& _bytes;
            size_t _at = 0;
        };

        // Reads a message from `socket`, waiting for it no longer than `SILENCE` nor once `running`
        // is false: its kind and body, or false.
        bool ReadMessage(tcp::socket& socket, std::atomic<bool> const& running, uint8& kind, std::vector<uint8>& body)
        {
            auto const until = std::chrono::steady_clock::now() + SILENCE;
            auto wait = [&](size_t count)
            {
                while (running && std::chrono::steady_clock::now() < until)
                {
                    boost::system::error_code error;
                    size_t const ready = socket.available(error);
                    if (error)
                        return false;
                    if (ready >= count)
                        return true;
                    std::this_thread::sleep_for(std::chrono::milliseconds(10));
                }
                return false;
            };
            std::array<uint8, 4> header{};
            if (!wait(4) || boost::asio::read(socket, boost::asio::buffer(header)) != 4)
                return false;
            uint32 const length = uint32(header[0]) | uint32(header[1]) << 8 | uint32(header[2]) << 16 | uint32(header[3]) << 24;
            if (length < 1 || length > MAX_IN || !wait(length))
                return false;
            std::vector<uint8> bytes(length);
            if (boost::asio::read(socket, boost::asio::buffer(bytes)) != length)
                return false;
            kind = bytes[0];
            body.assign(bytes.begin() + 1, bytes.end());
            return true;
        }

        void Send(tcp::socket& socket, std::vector<uint8> const& message)
        {
            boost::system::error_code ignored;
            boost::asio::write(socket, boost::asio::buffer(message), ignored);
        }

        void Refuse(tcp::socket& socket, std::string const& reason)
        {
            Writer writer(REFUSED);
            writer.String(reason);
            Send(socket, writer.Finish());
        }

        bool Known(uint8 action)
        {
            return action >= uint8(Action::Quests) && action <= uint8(Action::GameObject);
        }

        // Whether the spawn `spawnId` of `table` (`game_event_creature`, `game_event_gameobject`)
        // belongs to a game event, which spawns it itself.
        bool InEvent(char const* table, uint64 spawnId)
        {
            return bool(WorldDatabase.Query("SELECT guid FROM {} WHERE guid = {}", table, spawnId));
        }
    }

    Bridge& Bridge::Instance()
    {
        static Bridge bridge;
        return bridge;
    }

    void Bridge::Start()
    {
        if (!sConfigMgr->GetOption<bool>("UniwowBridge.Enable", true))
        {
            LOG_INFO("module", "uniwow-bridge: disabled");
            return;
        }
        _port = uint16(sConfigMgr->GetOption<uint32>("UniwowBridge.Port", 8092));
        _token = sConfigMgr->GetOption<std::string>("UniwowBridge.Token", "");
        if (_token.empty())
            LOG_WARN("module", "uniwow-bridge: no UniwowBridge.Token, every connection is refused");
        _running = true;
        _thread = std::thread([this] { Listen(); });
    }

    void Bridge::Stop()
    {
        _running = false;
        _made.notify_all();
        if (_thread.joinable())
            _thread.join();
    }

    // One editor at a time, on 127.0.0.1: HELLO with the token, then RELOAD as many times as it
    // wants, each answered by DONE once the world thread made it.
    void Bridge::Listen()
    {
        try
        {
            boost::asio::io_context io;
            tcp::acceptor acceptor(io);
            tcp::endpoint const endpoint(boost::asio::ip::make_address_v4("127.0.0.1"), _port);
            boost::system::error_code error;
            acceptor.open(endpoint.protocol(), error);
            if (!error)
                acceptor.bind(endpoint, error);
            if (!error)
                acceptor.listen(4, error);
            if (!error)
                acceptor.non_blocking(true, error);
            if (error)
            {
                LOG_ERROR("module", "uniwow-bridge: cannot listen on 127.0.0.1:{}: {}", _port, error.message());
                return;
            }
            LOG_INFO("module", "uniwow-bridge: listening on 127.0.0.1:{}", _port);
            while (_running)
            {
                tcp::socket socket(io);
                acceptor.accept(socket, error);
                if (error == boost::asio::error::would_block || error == boost::asio::error::try_again)
                {
                    std::this_thread::sleep_for(std::chrono::milliseconds(100));
                    continue;
                }
                if (error)
                    continue;
                socket.non_blocking(false, error);
                uint8 kind = 0;
                std::vector<uint8> body;
                if (!ReadMessage(socket, _running, kind, body))
                    continue;
                Reader hello(body);
                uint16 version = 0;
                std::string token;
                if (kind != HELLO || !hello.U16(version) || !hello.String(token, 256) || !hello.Done())
                    continue;
                if (version != VERSION)
                {
                    Refuse(socket, "this bridge speaks the version 1 of its protocol");
                    continue;
                }
                if (_token.empty() || token != _token)
                {
                    Refuse(socket, "wrong token");
                    continue;
                }
                {
                    Writer welcome(WELCOME);
                    welcome.U16(VERSION);
                    welcome.String(GitRevision::GetFullVersion());
                    Send(socket, welcome.Finish());
                }
                while (_running && ReadMessage(socket, _running, kind, body))
                {
                    Reader reader(body);
                    uint32 id = 0;
                    uint16 count = 0;
                    if (kind != RELOAD || !reader.U32(id) || !reader.U16(count) || count > MAX_DEMANDS)
                        break;
                    auto request = std::make_shared<Request>();
                    bool read = true;
                    for (uint16 index = 0; index < count && read; ++index)
                    {
                        uint8 action = 0;
                        uint64 value = 0;
                        read = reader.U8(action) && reader.U64(value) && Known(action);
                        request->demands.push_back({ Action(action), value });
                    }
                    if (!read || !reader.Done())
                        break;
                    bool const made = Make(request, WAIT_SECONDS);
                    Writer done(DONE);
                    done.U32(id);
                    done.U16(uint16(request->demands.size()));
                    for (size_t index = 0; index < request->demands.size(); ++index)
                    {
                        Outcome const outcome = made && index < request->outcomes.size()
                            ? request->outcomes[index]
                            : Outcome{ false, "the world did not make it in time" };
                        done.U8(outcome.done ? 1 : 0);
                        done.String(outcome.said);
                    }
                    Send(socket, done.Finish());
                }
            }
        }
        catch (std::exception const& error)
        {
            LOG_ERROR("module", "uniwow-bridge: {}", error.what());
        }
    }

    bool Bridge::Make(std::shared_ptr<Request> const& request, uint32 seconds)
    {
        std::unique_lock<std::mutex> lock(_lock);
        _queue.push_back(request);
        return _made.wait_for(lock, std::chrono::seconds(seconds), [&] { return request->made || !_running; })
            && request->made;
    }

    void Bridge::Update()
    {
        std::deque<std::shared_ptr<Request>> queued;
        {
            std::lock_guard<std::mutex> lock(_lock);
            queued.swap(_queue);
        }
        if (queued.empty())
            return;
        for (auto const& request : queued)
        {
            for (Demand const& demand : request->demands)
            {
                try
                {
                    request->outcomes.push_back(MakeOne(demand));
                }
                catch (std::exception const& error)
                {
                    request->outcomes.push_back({ false, error.what() });
                }
            }
            LOG_INFO("module", "uniwow-bridge: {} demands of the editor made", request->demands.size());
        }
        {
            std::lock_guard<std::mutex> lock(_lock);
            for (auto const& request : queued)
                request->made = true;
        }
        _made.notify_all();
    }

    Outcome Bridge::MakeOne(Demand const& demand)
    {
        switch (demand.action)
        {
            case Action::Quests:
            {
                // As `.reload all quest`, and the texts in every language.
                sObjectMgr->LoadQuestGreetings();
                sObjectMgr->LoadQuestAreaTriggers();
                sObjectMgr->LoadQuestPOI();
                sObjectMgr->LoadQuests();
                sObjectMgr->LoadGameObjectForQuests();
                sObjectMgr->LoadQuestLocales();
                sObjectMgr->LoadQuestOfferRewardLocale();
                sObjectMgr->LoadQuestRequestItemsLocale();
                sObjectMgr->LoadQuestStartersAndEnders();
                sPoolMgr->ReSpawnPoolQuests();
                // The marks over the givers seen again by the players in the world.
                for (auto const& [id, session] : sWorldSessionMgr->GetAllSessions())
                    if (Player* player = session ? session->GetPlayer() : nullptr)
                        if (player->IsInWorld())
                        {
                            // The marks of the game objects, and the "!" and "?" of the creatures
                            // each sees: the client asks them only as a giver comes into sight.
                            player->UpdateForQuestWorldObjects();
                            player->SendQuestGiverStatusMultiple();
                        }
                return { true, "the quests loaded again" };
            }
            case Action::Path:
                sWaypointMgr->ReloadPath(uint32(demand.value));
                return { true, Acore::StringFormat("the path {} loaded again", demand.value) };
            case Action::Addons:
                sObjectMgr->LoadCreatureAddons();
                return { true, "the addons of the creatures loaded again" };
            case Action::Formations:
                sFormationMgr->LoadCreatureFormations();
                return { true, "the groups of creatures loaded again" };
            case Action::Creature:
            {
                ObjectGuid::LowType const spawnId = ObjectGuid::LowType(demand.value);
                // Taken away where it runs now, on the map of its row as the server had it.
                uint32 taken = 0;
                if (CreatureData const* before = sObjectMgr->GetCreatureData(spawnId))
                {
                    if (Map* map = sMapMgr->FindBaseNonInstanceMap(before->mapid))
                    {
                        std::vector<Creature*> running;
                        auto const bounds = map->GetCreatureBySpawnIdStore().equal_range(spawnId);
                        for (auto itr = bounds.first; itr != bounds.second; ++itr)
                            running.push_back(itr->second);
                        for (Creature* creature : running)
                        {
                            creature->CombatStop();
                            creature->AddObjectToRemoveList();
                            ++taken;
                        }
                        map->RemoveCreatureRespawnTime(spawnId);
                    }
                    sObjectMgr->DeleteCreatureData(spawnId);
                }
                CreatureData const* data = sObjectMgr->LoadCreatureDataFromDB(spawnId);
                if (!data)
                    return { true, Acore::StringFormat("the creature {} taken away ({} running)", spawnId, taken) };
                if (sPoolMgr->IsPartOfAPool<Creature>(spawnId) || InEvent("game_event_creature", spawnId))
                    return { true, Acore::StringFormat("the creature {} read again; its pool or game event spawns it", spawnId) };
                Map* map = sMapMgr->FindBaseNonInstanceMap(data->mapid);
                if (!map)
                    return { true, Acore::StringFormat("the creature {} read again; seen once its instance is made", spawnId) };
                Creature* creature = new Creature();
                if (!creature->LoadCreatureFromDB(spawnId, map, true, true))
                {
                    delete creature;
                    return { false, Acore::StringFormat("the creature {} is read, not spawned", spawnId) };
                }
                sObjectMgr->AddCreatureToGrid(spawnId, data);
                return { true, Acore::StringFormat("the creature {} spawned again", spawnId) };
            }
            case Action::GameObject:
            {
                ObjectGuid::LowType const spawnId = ObjectGuid::LowType(demand.value);
                uint32 taken = 0;
                if (GameObjectData const* before = sObjectMgr->GetGameObjectData(spawnId))
                {
                    if (Map* map = sMapMgr->FindBaseNonInstanceMap(before->mapid))
                    {
                        std::vector<GameObject*> running;
                        auto const bounds = map->GetGameObjectBySpawnIdStore().equal_range(spawnId);
                        for (auto itr = bounds.first; itr != bounds.second; ++itr)
                            running.push_back(itr->second);
                        for (GameObject* object : running)
                        {
                            object->SetRespawnTime(0);
                            object->Delete();
                            ++taken;
                        }
                        map->RemoveGORespawnTime(spawnId);
                    }
                    sObjectMgr->DeleteGOData(spawnId);
                }
                GameObjectData const* data = sObjectMgr->LoadGameObjectDataFromDB(spawnId);
                if (!data)
                    return { true, Acore::StringFormat("the game object {} taken away ({} running)", spawnId, taken) };
                if (sPoolMgr->IsPartOfAPool<GameObject>(spawnId) || InEvent("game_event_gameobject", spawnId))
                    return { true, Acore::StringFormat("the game object {} read again; its pool or game event spawns it", spawnId) };
                Map* map = sMapMgr->FindBaseNonInstanceMap(data->mapid);
                GameObjectTemplate const* objectInfo = sObjectMgr->GetGameObjectTemplate(data->id);
                if (!map || !objectInfo)
                    return { true, Acore::StringFormat("the game object {} read again; seen once its map is made", spawnId) };
                GameObject* object = sObjectMgr->IsGameObjectStaticTransport(objectInfo->entry) ? new StaticTransport() : new GameObject();
                if (!object->LoadGameObjectFromDB(spawnId, map, true))
                {
                    delete object;
                    return { false, Acore::StringFormat("the game object {} is read, not spawned", spawnId) };
                }
                sObjectMgr->AddGameobjectToGrid(spawnId, data);
                return { true, Acore::StringFormat("the game object {} spawned again", spawnId) };
            }
        }
        return { false, "an unknown demand" };
    }
}
