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
#include "WorldPacket.h"
#include "WorldSession.h"
#include "WorldSessionMgr.h"

#include <boost/asio/io_context.hpp>
#include <boost/asio/ip/tcp.hpp>
#include <boost/asio/read.hpp>
#include <boost/asio/write.hpp>

#include <array>
#include <cmath>
#include <chrono>
#include <cstring>
#include <exception>

namespace UniwowBridge
{
    namespace
    {
        using boost::asio::ip::tcp;

        // Its version: 2 adds MOVE and the demand 8; an editor of the version 1 is welcome still.
        constexpr uint16 VERSION = 2;
        // The longest message from the editor: 256 demands, or 64 moves.
        constexpr uint32 MAX_IN = 4096;
        constexpr uint16 MAX_DEMANDS = 256;
        constexpr uint16 MAX_MOVES = 64;
        // How long a creature held waits for its editor before it is given its movement back.
        constexpr auto FORGOTTEN = std::chrono::seconds(5);
        // How long a connection may say nothing, and a request wait for the world.
        constexpr auto SILENCE = std::chrono::seconds(10);
        constexpr uint32 WAIT_SECONDS = 30;

        enum : uint8
        {
            HELLO = 1,
            RELOAD = 2,
            MOVE = 3,
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

            bool F32(float& value)
            {
                uint32 bits = 0;
                if (!U32(bits))
                    return false;
                std::memcpy(&value, &bits, sizeof(value));
                return std::isfinite(value);
            }

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
                    // Closed by the editor: nothing more comes, the next connection is taken at once.
                    if (ready == 0)
                    {
                        uint8 probe = 0;
                        boost::system::error_code peeked;
                        socket.non_blocking(true, peeked);
                        size_t const seen = socket.receive(boost::asio::buffer(&probe, 1), tcp::socket::message_peek, peeked);
                        boost::system::error_code ignored;
                        socket.non_blocking(false, ignored);
                        if (peeked == boost::asio::error::eof || (!peeked && seen == 0))
                            return false;
                    }
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
            return action >= uint8(Action::Quests) && action <= uint8(Action::Home);
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
                if (version < 1 || version > VERSION)
                {
                    Refuse(socket, "this bridge speaks the versions 1 and 2 of its protocol");
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
                    bool const moving = kind == MOVE && version >= 2;
                    if ((kind != RELOAD && !moving) || !reader.U32(id) || !reader.U16(count)
                        || count > (moving ? MAX_MOVES : MAX_DEMANDS))
                        break;
                    auto request = std::make_shared<Request>();
                    bool read = true;
                    for (uint16 index = 0; index < count && read; ++index)
                    {
                        if (moving)
                        {
                            Move move{};
                            uint8 letGo = 0;
                            read = reader.U32(move.map) && reader.U32(move.instance) && reader.U64(move.guid)
                                && reader.U8(letGo) && letGo <= 1 && reader.F32(move.x) && reader.F32(move.y)
                                && reader.F32(move.z) && reader.F32(move.orientation);
                            move.letGo = letGo == 1;
                            request->moves.push_back(move);
                            continue;
                        }
                        uint8 action = 0;
                        uint64 value = 0;
                        read = reader.U8(action) && reader.U64(value) && Known(action)
                            && (version >= 2 || action < uint8(Action::Home));
                        request->demands.push_back({ Action(action), value });
                    }
                    if (!read || !reader.Done())
                        break;
                    bool const made = Make(request, WAIT_SECONDS);
                    size_t const asked = request->demands.size() + request->moves.size();
                    Writer done(DONE);
                    done.U32(id);
                    done.U16(uint16(asked));
                    for (size_t index = 0; index < asked; ++index)
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
        LetGoForgotten();
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
            for (Move const& move : request->moves)
            {
                try
                {
                    request->outcomes.push_back(MakeMove(move));
                }
                catch (std::exception const& error)
                {
                    request->outcomes.push_back({ false, error.what() });
                }
            }
            // A drag sends ten moves a second: they are not told one by one.
            if (!request->demands.empty())
                LOG_INFO("module", "uniwow-bridge: {} demands of the editor made", request->demands.size());
        }
        {
            std::lock_guard<std::mutex> lock(_lock);
            for (auto const& request : queued)
                request->made = true;
        }
        _made.notify_all();
    }

    Outcome ForgetQuest(uint32 questId);

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
                            // As the client asks them (`Player::SendQuestGiverStatusMultiple` being
                            // for the session alone).
                            WorldPacket ask;
                            session->HandleQuestgiverStatusMultipleQuery(ask);
                        }
                return { true, "the quests loaded again" };
            }
            case Action::Path:
                return ReloadPath(uint32(demand.value));
            case Action::Home:
                return MoveHome(demand.value);
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
            case Action::ForgetQuest:
                return ForgetQuest(uint32(demand.value));
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

    // The path `pathId` loaded again (`Action::Path`), the creatures walking it set on it again. A
    // creature's movement holds the points of its path: it is let go of them before they are
    // replaced, and walks the new ones from their first.
    Outcome Bridge::ReloadPath(uint32 pathId)
    {
        std::vector<Creature*> walkers;
        sMapMgr->DoForAllMaps([&](Map* map)
        {
            for (auto const& [spawnId, creature] : map->GetCreatureBySpawnIdStore())
                if (creature->IsInWorld() && creature->GetWaypointPath() == pathId)
                    walkers.push_back(creature);
        });
        for (Creature* creature : walkers)
        {
            creature->GetMotionMaster()->Clear(false);
            creature->GetMotionMaster()->MoveIdle();
        }
        sWaypointMgr->ReloadPath(pathId);
        uint32 walking = 0;
        for (Creature* creature : walkers)
        {
            // One the editor holds is given its movement once let go.
            if (!creature->IsAlive() || _held.count(creature->GetGUID().GetRawValue()))
                continue;
            creature->GetMotionMaster()->Initialize();
            ++walking;
        }
        return { true, Acore::StringFormat("the path {} loaded again, {} creatures walking it again", pathId, walking) };
    }

    // The spawn point of the creature `spawnId` read again from its row (`Action::Home`): the
    // creatures of it running keep their place and take it as their home; read again whole, as
    // `Action::Creature` does, when its row is gone or on another map.
    Outcome Bridge::MoveHome(uint64 spawnId)
    {
        ObjectGuid::LowType const id = ObjectGuid::LowType(spawnId);
        CreatureData const* before = sObjectMgr->GetCreatureData(id);
        QueryResult row = WorldDatabase.Query(
            "SELECT map, position_x, position_y, position_z, orientation FROM creature WHERE guid = {}", id);
        if (!before || !row || row->Fetch()[0].Get<uint16>() != before->mapid)
            return MakeOne({ Action::Creature, spawnId });
        Field* fields = row->Fetch();
        float const x = fields[1].Get<float>();
        float const y = fields[2].Get<float>();
        float const z = fields[3].Get<float>();
        float const orientation = fields[4].Get<float>();
        // Its place in the grids of the spawns follows it.
        sObjectMgr->RemoveCreatureFromGrid(id, before);
        CreatureData& data = sObjectMgr->NewOrExistCreatureData(id);
        data.posX = x;
        data.posY = y;
        data.posZ = z;
        data.orientation = orientation;
        sObjectMgr->AddCreatureToGrid(id, &data);
        uint32 running = 0;
        if (Map* map = sMapMgr->FindBaseNonInstanceMap(data.mapid))
        {
            auto const bounds = map->GetCreatureBySpawnIdStore().equal_range(id);
            for (auto itr = bounds.first; itr != bounds.second; ++itr)
            {
                itr->second->SetHomePosition(x, y, z, orientation);
                ++running;
            }
        }
        return { true, Acore::StringFormat("the spawn point of the creature {} moved; {} running go home there", id, running) };
    }

    // The creature of `move` put where the editor drags it, live, nothing written: held still while
    // it is dragged, given its movement back once let go.
    Outcome Bridge::MakeMove(Move const& move)
    {
        Map* map = sMapMgr->FindMap(move.map, move.instance);
        Creature* creature = map ? map->GetCreature(ObjectGuid(move.guid)) : nullptr;
        if (!creature || !creature->IsInWorld())
        {
            _held.erase(move.guid);
            return { false, "the creature is no longer in the world" };
        }
        if (!_held.count(move.guid))
        {
            creature->StopMoving();
            creature->GetMotionMaster()->Clear(false);
            creature->GetMotionMaster()->MoveIdle();
        }
        creature->NearTeleportTo(move.x, move.y, move.z, move.orientation);
        if (move.letGo)
        {
            _held.erase(move.guid);
            creature->GetMotionMaster()->Initialize();
            return { true, Acore::StringFormat("{} put down, moving again", creature->GetName()) };
        }
        _held[move.guid] = { move.map, move.instance, std::chrono::steady_clock::now() };
        return { true, Acore::StringFormat("{} held", creature->GetName()) };
    }

    // The creatures held that their editor has not moved for `FORGOTTEN` given their movement back:
    // an editor closed while it dragged one leaves none standing still.
    void Bridge::LetGoForgotten()
    {
        auto const now = std::chrono::steady_clock::now();
        for (auto itr = _held.begin(); itr != _held.end();)
        {
            if (now - itr->second.last < FORGOTTEN)
            {
                ++itr;
                continue;
            }
            if (Map* map = sMapMgr->FindMap(itr->second.map, itr->second.instance))
                if (Creature* creature = map->GetCreature(ObjectGuid(itr->first)))
                    if (creature->IsInWorld())
                        creature->GetMotionMaster()->Initialize();
            itr = _held.erase(itr);
        }
    }

    // The quest `questId` taken from every character (`Action::ForgetQuest`).
    Outcome ForgetQuest(uint32 questId)
    {
        uint32 online = 0;
        for (auto const& [id, session] : sWorldSessionMgr->GetAllSessions())
        {
            Player* player = session ? session->GetPlayer() : nullptr;
            if (!player)
                continue;
            bool held = false;
            for (uint8 slot = 0; slot < MAX_QUEST_LOG_SIZE; ++slot)
            {
                if (player->GetQuestSlotQuestId(slot) == questId)
                {
                    player->SetQuestSlot(slot, 0);
                    player->TakeQuestSourceItem(questId, false);
                    held = true;
                }
            }
            held |= player->GetQuestRewardStatus(questId) || player->GetQuestStatus(questId) != QUEST_STATUS_NONE;
            player->RemoveRewardedQuest(questId);
            player->RemoveActiveQuest(questId, false);
            if (held)
                ++online;
        }
        // Those not in the world, and what those in it save: their status in the database.
        CharacterDatabaseTransaction trans = CharacterDatabase.BeginTransaction();
        for (char const* table : { "character_queststatus", "character_queststatus_rewarded", "character_queststatus_daily",
                 "character_queststatus_weekly", "character_queststatus_monthly", "character_queststatus_seasonal" })
            trans->Append(Acore::StringFormat("DELETE FROM `{}` WHERE `quest` = {}", table, questId));
        CharacterDatabase.CommitTransaction(trans);
        return { true, Acore::StringFormat("the quest {} taken from every character ({} in the world held it)", questId, online) };
    }
}
