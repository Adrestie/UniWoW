// SPDX-License-Identifier: GPL-2.0-or-later
// The hooks of AzerothCore the observer runs from.

#include "Observer.h"

#include "Log.h"
#include "ScriptMgr.h"

#include <exception>

namespace
{
    class ObserverWorld : public WorldScript
    {
    public:
        ObserverWorld() : WorldScript("UniwowObserverWorld", { WORLDHOOK_ON_STARTUP, WORLDHOOK_ON_SHUTDOWN }) { }

        void OnStartup() override
        {
            UniwowObserver::Observer::Instance().Start();
        }

        void OnShutdown() override
        {
            UniwowObserver::Observer::Instance().Stop();
        }
    };

    class ObserverMaps : public AllMapScript
    {
    public:
        ObserverMaps() : AllMapScript("UniwowObserverMaps", { ALLMAPHOOK_ON_MAP_UPDATE }) { }

        // On the thread updating `map`, at the end of its update.
        void OnMapUpdate(Map* map, uint32 /*diff*/) override
        {
            try
            {
                UniwowObserver::Observer::Instance().Update(map);
            }
            catch (std::exception const& error)
            {
                LOG_ERROR("module", "uniwow-observer: {}", error.what());
            }
            catch (...)
            {
                LOG_ERROR("module", "uniwow-observer: an unknown error in the update of a map");
            }
        }
    };
}

void AddUniwowObserverScripts()
{
    new ObserverWorld();
    new ObserverMaps();
}
