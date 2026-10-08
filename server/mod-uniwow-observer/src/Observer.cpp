// SPDX-License-Identifier: GPL-2.0-or-later

#include "Observer.h"
#include "Server.h"

#include "CellImpl.h"
#include "Config.h"
#include "Creature.h"
#include "GameEventMgr.h"
#include "GameObject.h"
#include "GridDefines.h"
#include "Log.h"
#include "Map.h"
#include "MoveSpline.h"
#include "Player.h"
#include "PoolMgr.h"

#include <exception>

namespace UniwowObserver
{
    namespace
    {
        double Milliseconds(Clock::duration duration)
        {
            return std::chrono::duration<double, std::milli>(duration).count();
        }

        // The game events of the spawns, by the guid of their row: `GameEventMgr` lists the spawns
        // of each event, never the event of a spawn.
        template<class Lists>
        void IndexEvents(Lists const& lists, std::unordered_map<uint32, int16>& events)
        {
            int32 const count = int32(sGameEventMgr->GetEventMap().size());
            for (size_t index = 0; index < lists.size(); ++index)
            {
                int16 const event = int16(int32(index) - count + 1);
                for (ObjectGuid::LowType spawn : lists[index])
                    events.emplace(spawn, event);
            }
        }

        std::string Name(std::string const& name)
        {
            if (name.size() <= NAME_BYTES)
                return name;
            // Cut on a whole character of UTF-8.
            size_t end = NAME_BYTES;
            while (end > 0 && (uint8(name[end]) & 0xC0) == 0x80)
                --end;
            return name.substr(0, end);
        }
    }

    // Collects the creatures, game objects and players of a zone; the grids give every object of
    // the cells the circle touches, the circle is applied here.
    struct Visitor
    {
        Visitor(Observer const& observer, Zone const& zone) : _observer(observer), _zone(zone) { }

        void Visit(CreatureMapType& objects)
        {
            for (auto itr = objects.begin(); itr != objects.end(); ++itr)
            {
                Creature* creature = itr->GetSource();
                Record record;
                if (!Place(creature, record))
                    continue;
                record.kind = Kind::Creature;
                record.entry = creature->GetEntry();
                record.spawn = creature->GetSpawnId();
                if (creature->IsSummon() || !record.spawn)
                    record.flags |= FLAG_TEMPORARY;
                if (record.spawn)
                {
                    record.pool = sPoolMgr->IsPartOfAPool<Creature>(record.spawn);
                    record.event = _observer.EventOf(Kind::Creature, record.spawn);
                }
                ReadUnit(creature, record);
                records.push_back(std::move(record));
            }
        }

        void Visit(GameObjectMapType& objects)
        {
            for (auto itr = objects.begin(); itr != objects.end(); ++itr)
            {
                GameObject* object = itr->GetSource();
                Record record;
                if (!Place(object, record))
                    continue;
                record.kind = Kind::GameObject;
                record.entry = object->GetEntry();
                record.spawn = object->GetSpawnId();
                if (!record.spawn)
                    record.flags |= FLAG_TEMPORARY;
                if (!object->isSpawned())
                    record.flags |= FLAG_DEAD;
                if (record.spawn)
                {
                    record.pool = sPoolMgr->IsPartOfAPool<GameObject>(record.spawn);
                    record.event = _observer.EventOf(Kind::GameObject, record.spawn);
                }
                record.display = object->GetDisplayId();
                G3D::Quat const& rotation = object->GetWorldRotation();
                record.rotation = { rotation.x, rotation.y, rotation.z, rotation.w };
                record.state = uint8(object->GetGoState());
                records.push_back(std::move(record));
            }
        }

        void Visit(PlayerMapType& objects)
        {
            for (auto itr = objects.begin(); itr != objects.end(); ++itr)
            {
                Player* player = itr->GetSource();
                Record record;
                if (!Place(player, record))
                    continue;
                record.kind = Kind::Player;
                if (player->IsGameMaster())
                    record.flags |= FLAG_GAME_MASTER;
                ReadUnit(player, record);
                records.push_back(std::move(record));
            }
        }

        template<class Other> void Visit(GridRefMgr<Other>&) { }

        std::vector<Record> records;

    private:
        // What every object gives; false when it lies outside the circle.
        bool Place(WorldObject* object, Record& record) const
        {
            float const dx = object->GetPositionX() - _zone.x;
            float const dy = object->GetPositionY() - _zone.y;
            record.distance = dx * dx + dy * dy;
            if (record.distance > _zone.radius * _zone.radius)
                return false;
            record.guid = object->GetGUID().GetRawValue();
            record.phase = object->GetPhaseMask();
            record.x = object->GetPositionX();
            record.y = object->GetPositionY();
            record.z = object->GetPositionZ();
            record.orientation = object->GetOrientation();
            record.scale = object->GetObjectScale();
            record.name = Name(object->GetName());
            return true;
        }

        static void ReadUnit(Unit* unit, Record& record)
        {
            record.display = unit->GetDisplayId();
            if (!unit->IsAlive())
                record.flags |= FLAG_DEAD;
            if (unit->IsWalking())
                record.flags |= FLAG_WALKING;
            if (unit->IsFlying())
                record.flags |= FLAG_FLYING;
            Movement::MoveSpline const* move = unit->movespline;
            if (!move || !move->Initialized() || move->Finalized())
                return;
            record.flags |= FLAG_MOVING;
            Movement::MoveSpline::MySpline const& spline = move->_Spline();
            record.spline = move->GetId();
            if (spline.mode() == Movement::SplineBase::ModeCatmullrom)
                record.splineFlags |= SPLINE_CATMULL_ROM;
            if (spline.isCyclic())
                record.splineFlags |= SPLINE_CYCLIC;
            if (move->isFalling())
                record.splineFlags |= SPLINE_FALLING;
            record.elapsed = uint32(std::max(move->timePassed(), 0));
            // The points gone through, from first() to last(): Catmull-Rom has one more for control
            // at each end. A long spline is sent from the segment it is on.
            int32 const first = spline.first();
            int32 const last = spline.last();
            int32 const most = int32(PATH_POINTS);
            int32 const start = last - first + 1 > most ? std::clamp(move->_currentSplineIdx(), first, last - most + 1) : first;
            int32 const end = std::min(last, start + most - 1);
            int32 const zero = spline.length(first);
            record.path.reserve(size_t(end - start + 1));
            for (int32 index = start; index <= end; ++index)
            {
                G3D::Vector3 const& point = spline.getPoint(index);
                record.path.push_back({ point.x, point.y, point.z, uint32(std::max(spline.length(index) - zero, 0)) });
            }
        }

        Observer const& _observer;
        Zone const& _zone;
    };

    Observer& Observer::Instance()
    {
        static Observer instance;
        return instance;
    }

    void Observer::Start()
    {
        _settings.enable = sConfigMgr->GetOption<bool>("UniwowObserver.Enable", true);
        _settings.port = uint16(sConfigMgr->GetOption<uint32>("UniwowObserver.Port", 8087));
        _settings.token = sConfigMgr->GetOption<std::string>("UniwowObserver.Token", "");
        _settings.rate = std::clamp<uint32>(sConfigMgr->GetOption<uint32>("UniwowObserver.Rate", 10), 1, 20);
        _settings.maxRadius = std::clamp(sConfigMgr->GetOption<float>("UniwowObserver.MaxRadius", 533.0f), 1.0f, float(SIZE_OF_GRIDS));
        _settings.maxEntities = std::clamp<uint32>(sConfigMgr->GetOption<uint32>("UniwowObserver.MaxEntities", 2000), 1, 100000);
        _settings.maxConnections = std::clamp<uint32>(sConfigMgr->GetOption<uint32>("UniwowObserver.MaxConnections", 4), 1, 16);
        _settings.heartbeat = std::clamp<uint32>(sConfigMgr->GetOption<uint32>("UniwowObserver.Heartbeat", 10), 2, 600);
        _settings.keepAlive = sConfigMgr->GetOption<bool>("UniwowObserver.KeepAlive", true);
        _settings.statsInterval = sConfigMgr->GetOption<uint32>("UniwowObserver.StatsInterval", 0);
        _period = std::chrono::duration_cast<Clock::duration>(std::chrono::milliseconds(1000 / _settings.rate));
        if (!_settings.enable)
        {
            LOG_INFO("module", "uniwow-observer: disabled by UniwowObserver.Enable");
            return;
        }
        if (_settings.token.empty())
            LOG_WARN("module", "uniwow-observer: UniwowObserver.Token is empty, every connection is refused");

        IndexEvents(sGameEventMgr->GameEventCreatureGuids, _creatureEvents);
        IndexEvents(sGameEventMgr->GameEventGameobjectGuids, _gameObjectEvents);
        _subscriptions.store(std::make_shared<Subscriptions const>());

        try
        {
            _server = std::make_unique<Server>(_settings);
            if (!_server->Start())
            {
                _server.reset();
                return;
            }
        }
        catch (std::exception const& error)
        {
            LOG_ERROR("module", "uniwow-observer: not started: {}", error.what());
            _server.reset();
            return;
        }
        _running.store(true, std::memory_order_release);
        LOG_INFO("module", "uniwow-observer: listening on 127.0.0.1:{}", _settings.port);
    }

    void Observer::Stop()
    {
        _running.store(false, std::memory_order_release);
        if (_server)
        {
            _server->Stop();
            _server.reset();
        }
        _subscriptions.store(std::make_shared<Subscriptions const>());
    }

    void Observer::Publish(Subscriptions subscriptions)
    {
        _subscriptions.store(std::make_shared<Subscriptions const>(std::move(subscriptions)));
    }

    void Observer::Update(Map* map)
    {
        if (!_running.load(std::memory_order_acquire))
            return;
        // The container of the instances of a map, which has none of their objects.
        if (map->Instanceable() && !map->GetInstanceId())
            return;
        std::shared_ptr<Subscriptions const> subscriptions = _subscriptions.load();
        if (!subscriptions)
            return;
        for (std::shared_ptr<Subscription> const& subscription : *subscriptions)
        {
            try
            {
                Update(map, *subscription);
            }
            catch (std::exception const& error)
            {
                LOG_ERROR("module", "uniwow-observer: map {} not read: {}", map->GetId(), error.what());
            }
            catch (...)
            {
                LOG_ERROR("module", "uniwow-observer: map {} not read", map->GetId());
            }
        }
    }

    void Observer::Update(Map* map, Subscription& subscription)
    {
        Clock::time_point const now = Clock::now();
        Zone zone;
        uint64 version = 0;
        uint64 generation = 0;
        bool due = false;
        bool load = false;
        std::shared_ptr<std::vector<ObjectGuid> const> kept;
        {
            std::lock_guard<std::mutex> guard(subscription.lock);
            if (subscription.closed || subscription.zone.map != map->GetId() || subscription.zone.instance != map->GetInstanceId())
                return;
            zone = subscription.zone;
            version = subscription.version;
            generation = subscription.generation;
            kept = subscription.kept;
            // At once on a new map; else at the pace of the protocol, a zone moved read after half
            // of it at the soonest however often it moves.
            Clock::duration const since = now - subscription.lastRead;
            due = !subscription.read || subscription.readGeneration != generation || since >= _period
                || (subscription.readVersion != version && since >= _period / 2);
            load = !subscription.loaded || subscription.loadedVersion != version;
        }

        // The objects of the last reading put back in the update list, as the sight of a player
        // does: without it they stop being updated once no player is near.
        double keepTime = -1.0;
        if (_settings.keepAlive && kept)
        {
            Clock::time_point const start = Clock::now();
            for (ObjectGuid const& guid : *kept)
            {
                WorldObject* object = guid.IsGameObject() ? static_cast<WorldObject*>(map->GetGameObject(guid)) : map->GetCreature(guid);
                if (object)
                    map->AddObjectToPendingUpdateList(object);
            }
            keepTime = Milliseconds(Clock::now() - start);
        }

        double readTime = -1.0;
        auto reading = std::make_shared<Reading>();
        auto found = std::make_shared<std::vector<ObjectGuid>>();
        if (due)
        {
            Clock::time_point const start = Clock::now();
            // Grids stay loaded once loaded: those of a zone are loaded when it changes.
            if (load)
                map->LoadGridsInRange(Position(zone.x, zone.y, zone.z), zone.radius);
            Read(map, zone, *reading, *found);
            readTime = Milliseconds(Clock::now() - start);
        }

        std::lock_guard<std::mutex> guard(subscription.lock);
        if (keepTime >= 0.0)
            subscription.keeps.Add(keepTime);
        if (!due || subscription.closed || subscription.generation != generation)
            return;
        subscription.reads.Add(readTime);
        reading->sequence = ++subscription.sequence;
        subscription.reading = std::move(reading);
        subscription.kept = std::move(found);
        subscription.read = true;
        subscription.readVersion = version;
        subscription.readGeneration = generation;
        subscription.lastRead = now;
        subscription.loaded = true;
        subscription.loadedVersion = version;
        subscription.state = State::Active;
    }

    void Observer::Read(Map* map, Zone const& zone, Reading& reading, std::vector<ObjectGuid>& kept) const
    {
        Visitor visitor(*this, zone);
        Cell::VisitObjects(zone.x, zone.y, map, visitor, zone.radius);
        std::vector<Record>& records = visitor.records;
        if (records.size() > _settings.maxEntities)
        {
            auto const nearer = [](Record const& a, Record const& b) { return a.distance < b.distance; };
            std::nth_element(records.begin(), records.begin() + _settings.maxEntities, records.end(), nearer);
            records.resize(_settings.maxEntities);
        }
        // Kept updated: the creatures and game objects sent, the nearest, never more than sent.
        kept.clear();
        kept.reserve(records.size());
        for (Record const& record : records)
            if (record.kind != Kind::Player)
                kept.push_back(ObjectGuid(record.guid));
        reading.records = std::move(records);
    }

    int16 Observer::EventOf(Kind kind, uint32 spawn) const
    {
        auto const& events = kind == Kind::Creature ? _creatureEvents : _gameObjectEvents;
        auto const found = events.find(spawn);
        return found == events.end() ? int16(0) : found->second;
    }
}
