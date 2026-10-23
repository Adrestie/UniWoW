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

#include <algorithm>
#include <array>
#include <cmath>
#include <ctime>
#include <optional>
#include <chrono>
#include <cstring>
#include <exception>

namespace UniwowBridge
{
    namespace
    {
        using boost::asio::ip::tcp;

        // Its version: 2 adds MOVE and the demand 8, 3 adds SET and when the server started; an
        // editor of an earlier version is welcome still.
        constexpr uint16 VERSION = 3;
        // The longest message from the editor: 256 demands, 64 moves, or 256 sets of 1,000 points
        // at most a path.
        constexpr uint32 MAX_IN = 65536;
        constexpr uint16 MAX_DEMANDS = 256;
        constexpr uint16 MAX_MOVES = 64;
        constexpr uint16 MAX_SETS = 256;
        constexpr uint16 MAX_POINTS = 1000;
        constexpr uint16 MAX_MEMBERS = 256;
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
            SET = 4,
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
            void U64(uint64 value) { Little(value, 8); }

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

        // The paths of `WaypointMgr`, which keeps them private and loads them from the database
        // alone: reached through the explicit instantiation of a template, which may name a private
        // member, for a path to be set in memory without touching the core.
        struct WaypointStore
        {
            using type = std::unordered_map<uint32, WaypointPath> WaypointMgr::*;
            friend type Paths(WaypointStore);
        };

        template <typename Tag, typename Tag::type Member>
        struct Reach
        {
            friend typename Tag::type Paths(Tag) { return Member; }
        };

        template struct Reach<WaypointStore, &WaypointMgr::_waypointStore>;

        // The creatures walking the path `pathId`, everywhere.
        std::vector<Creature*> WalkersOf(uint32 pathId)
        {
            std::vector<Creature*> walkers;
            sMapMgr->DoForAllMaps([&](Map* map)
            {
                for (auto const& [spawnId, creature] : map->GetCreatureBySpawnIdStore())
                    if (creature->IsInWorld() && creature->GetWaypointPath() == pathId)
                        walkers.push_back(creature);
            });
            return walkers;
        }

        // A creature's movement let go of its path, which holds its points, before they are replaced.
        void LetGoOfPath(Creature* creature)
        {
            creature->GetMotionMaster()->Clear(false);
            creature->GetMotionMaster()->MoveIdle();
        }

        // The creatures running of the spawn `spawnId`, on the map of its row.
        std::vector<Creature*> RunningOf(ObjectGuid::LowType spawnId, uint32 mapId)
        {
            std::vector<Creature*> running;
            if (Map* map = sMapMgr->FindBaseNonInstanceMap(mapId))
            {
                auto const bounds = map->GetCreatureBySpawnIdStore().equal_range(spawnId);
                for (auto itr = bounds.first; itr != bounds.second; ++itr)
                    running.push_back(itr->second);
            }
            return running;
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
        _started = uint64(std::time(nullptr));
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
                    Refuse(socket, "this bridge speaks the versions 1 to 3 of its protocol");
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
                    welcome.U64(_started);
                    Send(socket, welcome.Finish());
                }
                while (_running && ReadMessage(socket, _running, kind, body))
                {
                    Reader reader(body);
                    uint32 id = 0;
                    uint16 count = 0;
                    bool const moving = kind == MOVE && version >= 2;
                    bool const setting = kind == SET && version >= 3;
                    if ((kind != RELOAD && !moving && !setting) || !reader.U32(id) || !reader.U16(count)
                        || count > (moving ? MAX_MOVES : setting ? MAX_SETS : MAX_DEMANDS))
                        break;
                    auto request = std::make_shared<Request>();
                    bool read = true;
                    for (uint16 index = 0; index < count && read; ++index)
                    {
                        if (setting)
                        {
                            Set set{};
                            read = reader.U8(set.what);
                            if (read && set.what == Set::HOME)
                                read = reader.U64(set.id) && reader.F32(set.x) && reader.F32(set.y)
                                    && reader.F32(set.z) && reader.F32(set.orientation);
                            else if (read && set.what == Set::PATH)
                            {
                                uint32 path = 0;
                                uint16 points = 0;
                                read = reader.U32(path) && reader.U16(points) && points <= MAX_POINTS;
                                set.id = path;
                                for (uint16 at = 0; at < points && read; ++at)
                                {
                                    PathPoint point{};
                                    uint8 faces = 0;
                                    uint8 run = 0;
                                    read = reader.F32(point.x) && reader.F32(point.y) && reader.F32(point.z)
                                        && reader.U8(faces) && reader.F32(point.orientation)
                                        && reader.U32(point.delay) && reader.U8(run);
                                    point.faces = faces == 1;
                                    point.run = run == 1;
                                    set.points.push_back(point);
                                }
                            }
                            else if (read && set.what == Set::WALKER)
                                read = reader.U64(set.id) && reader.U8(set.movement) && reader.U32(set.path);
                            else if (read && set.what == Set::GROUP)
                            {
                                uint16 members = 0;
                                read = reader.U64(set.id) && reader.U16(members) && members <= MAX_MEMBERS;
                                for (uint16 at = 0; at < members && read; ++at)
                                {
                                    GroupMember member{};
                                    read = reader.U64(member.guid) && reader.F32(member.distance)
                                        && reader.F32(member.angle) && reader.U16(member.ai);
                                    set.members.push_back(member);
                                }
                            }
                            else
                                read = false;
                            request->sets.push_back(std::move(set));
                            continue;
                        }
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
                    size_t const asked = request->demands.size() + request->sets.size() + request->moves.size();
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
            for (Set const& set : request->sets)
            {
                try
                {
                    request->outcomes.push_back(MakeSet(set));
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
        std::vector<Creature*> const walkers = WalkersOf(pathId);
        for (Creature* creature : walkers)
            LetGoOfPath(creature);
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
        return PlaceHome(spawnId, fields[1].Get<float>(), fields[2].Get<float>(), fields[3].Get<float>(),
            fields[4].Get<float>());
    }

    // The spawn point of the creature `spawnId` put at `x`, `y`, `z`, facing `orientation`, in the
    // server's memory: the creatures of it running keep their place and take it as their home.
    Outcome Bridge::PlaceHome(uint64 spawnId, float x, float y, float z, float orientation)
    {
        ObjectGuid::LowType const id = ObjectGuid::LowType(spawnId);
        CreatureData const* before = sObjectMgr->GetCreatureData(id);
        if (!before)
            return { false, Acore::StringFormat("the server knows no creature {}", id) };
        // Its place in the grids of the spawns follows it.
        sObjectMgr->RemoveCreatureFromGrid(id, before);
        CreatureData& data = sObjectMgr->NewOrExistCreatureData(id);
        data.posX = x;
        data.posY = y;
        data.posZ = z;
        data.orientation = orientation;
        sObjectMgr->AddCreatureToGrid(id, &data);
        std::vector<Creature*> const running = RunningOf(id, data.mapid);
        for (Creature* creature : running)
            creature->SetHomePosition(x, y, z, orientation);
        return { true, Acore::StringFormat("the spawn point of the creature {} moved; {} running go home there", id, running.size()) };
    }

    // A change of the editor set in the server's memory alone.
    Outcome Bridge::MakeSet(Set const& set)
    {
        switch (set.what)
        {
            case Set::HOME:
                return PlaceHome(set.id, set.x, set.y, set.z, set.orientation);
            case Set::PATH:
                return SetPath(uint32(set.id), set.points);
            case Set::WALKER:
                return SetWalker(set.id, set.movement, set.path);
            case Set::GROUP:
                return SetGroup(set.id, set.members);
        }
        return { false, "an unknown set" };
    }

    // The path `pathId` given `points` in the server's memory, none taking it away: its walkers
    // let go of it first, set on it again where they stand.
    Outcome Bridge::SetPath(uint32 pathId, std::vector<PathPoint> const& points)
    {
        std::vector<Creature*> const walkers = WalkersOf(pathId);
        for (Creature* creature : walkers)
            LetGoOfPath(creature);
        auto& paths = sWaypointMgr->*Paths(WaypointStore());
        if (points.empty())
            paths.erase(pathId);
        else
        {
            std::vector<WaypointNode> nodes;
            nodes.reserve(points.size());
            for (size_t at = 0; at < points.size(); ++at)
            {
                PathPoint const& point = points[at];
                std::optional<float> facing;
                if (point.faces)
                    facing = point.orientation;
                WaypointNode node(uint32(at + 1), point.x, point.y, point.z, facing, 0.f, point.delay);
                node.MoveType = point.run ? WAYPOINT_MOVE_TYPE_RUN : WAYPOINT_MOVE_TYPE_WALK;
                nodes.push_back(std::move(node));
            }
            paths[pathId] = WaypointPath(pathId, std::move(nodes));
        }
        uint32 walking = 0;
        for (Creature* creature : walkers)
        {
            if (!creature->IsAlive() || _held.count(creature->GetGUID().GetRawValue()))
                continue;
            creature->GetMotionMaster()->Initialize();
            ++walking;
        }
        return { true, Acore::StringFormat("the path {} set in memory, {} points, {} creatures walking it", pathId,
            points.size(), walking) };
    }

    // The creature `spawnId` given its movement (`MovementType`) and path in the server's memory:
    // those of it running moving so at once.
    Outcome Bridge::SetWalker(uint64 spawnId, uint8 movement, uint32 pathId)
    {
        ObjectGuid::LowType const id = ObjectGuid::LowType(spawnId);
        if (!sObjectMgr->GetCreatureData(id) || movement >= MAX_DB_MOTION_TYPE)
            return { false, Acore::StringFormat("the server knows no creature {}, or no movement {}", id, movement) };
        CreatureData& data = sObjectMgr->NewOrExistCreatureData(id);
        data.movementType = movement;
        // Made to wander, within a few yards: none would not wander, as the editor writes it.
        if (movement == RANDOM_MOTION_TYPE && data.wander_distance <= 0.f)
            data.wander_distance = 5.f;
        std::vector<Creature*> const running = RunningOf(id, data.mapid);
        for (Creature* creature : running)
        {
            if (movement == RANDOM_MOTION_TYPE)
                creature->SetWanderDistance(data.wander_distance);
            creature->LoadPath(pathId);
            creature->SetDefaultMovementType(MovementGeneratorType(movement));
            if (creature->IsAlive() && !_held.count(creature->GetGUID().GetRawValue()))
                creature->GetMotionMaster()->Initialize();
        }
        return { true, Acore::StringFormat("the creature {} moves so in memory ({} running)", id, running.size()) };
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

    // The group led by `leader` set in the server's memory as `members` (the leader among them), none
    // breaking it: each member taken out of the group it was in, the group's rows of the formations
    // replaced, the creatures running of them put in it, following their leader as it walks. The
    // formations loaded from the database (`Action::Formations`) do not put in their groups the
    // creatures running: done here.
    Outcome Bridge::SetGroup(uint64 leader, std::vector<GroupMember> const& members)
    {
        ObjectGuid::LowType const leaderId = ObjectGuid::LowType(leader);
        CreatureData const* leaderData = sObjectMgr->GetCreatureData(leaderId);
        if (!leaderData)
            return { false, Acore::StringFormat("the server knows no creature {}", leaderId) };
        auto& map = sFormationMgr->CreatureGroupMap;
        auto& groups = sFormationMgr->CreatureGroupMembers;
        // Those leaving: the members of its group before, and the new ones from theirs.
        std::vector<ObjectGuid::LowType> leaving = groups[leaderId];
        for (GroupMember const& member : members)
            leaving.push_back(ObjectGuid::LowType(member.guid));
        auto staying = [&](ObjectGuid::LowType guid)
        {
            return std::any_of(members.begin(), members.end(),
                [&](GroupMember const& member) { return ObjectGuid::LowType(member.guid) == guid; });
        };
        for (ObjectGuid::LowType guid : leaving)
        {
            if (CreatureData const* data = sObjectMgr->GetCreatureData(guid))
                for (Creature* creature : RunningOf(guid, data->mapid))
                {
                    if (CreatureGroup* group = creature->GetFormation())
                        sFormationMgr->RemoveCreatureFromGroup(group, creature);
                    // Out of the group, its formation movement kept it walking where its leader
                    // went: home again, then its own movement. One staying stops, to follow.
                    // The leader keeps walking its own round.
                    if (guid == leaderId || !creature->IsAlive() || creature->IsInCombat()
                        || _held.count(creature->GetGUID().GetRawValue()))
                        continue;
                    creature->GetMotionMaster()->Clear(false);
                    if (staying(guid))
                        creature->GetMotionMaster()->MoveIdle();
                    else
                        creature->GetMotionMaster()->MoveTargetedHome();
                }
            auto info = map.find(guid);
            if (info == map.end())
                continue;
            auto& before = groups[info->second.leaderGUID];
            before.erase(std::remove(before.begin(), before.end(), guid), before.end());
            map.erase(info);
        }
        groups.erase(leaderId);
        if (members.empty())
            return { true, Acore::StringFormat("the group of {} broken in memory", leaderId) };
        for (GroupMember const& member : members)
        {
            ObjectGuid::LowType const guid = ObjectGuid::LowType(member.guid);
            if (!sObjectMgr->GetCreatureData(guid))
                continue;
            FormationInfo info;
            info.leaderGUID = leaderId;
            info.groupAI = member.ai;
            if (guid != leaderId)
            {
                info.follow_dist = member.distance;
                info.follow_angle = member.angle * (static_cast<float>(M_PI) / 180);
            }
            map[guid] = info;
            groups[leaderId].push_back(guid);
        }
        // The leader put in first, its members after: they follow it as it walks.
        uint32 running = 0;
        std::vector<ObjectGuid::LowType> order = groups[leaderId];
        std::stable_partition(order.begin(), order.end(), [&](ObjectGuid::LowType guid) { return guid == leaderId; });
        for (ObjectGuid::LowType guid : order)
            if (CreatureData const* data = sObjectMgr->GetCreatureData(guid))
                for (Creature* creature : RunningOf(guid, data->mapid))
                {
                    sFormationMgr->AddCreatureToGroup(leaderId, creature);
                    ++running;
                }
        return { true, Acore::StringFormat("the group of {} set in memory, {} members, {} running", leaderId,
            members.size(), running) };
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
