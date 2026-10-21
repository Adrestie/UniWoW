// SPDX-License-Identifier: GPL-2.0-or-later
// The hooks of AzerothCore the bridge runs from.

#include "Bridge.h"

#include "Log.h"
#include "ScriptMgr.h"

#include <exception>

namespace
{
    class BridgeWorld : public WorldScript
    {
    public:
        BridgeWorld() : WorldScript("UniwowBridgeWorld", { WORLDHOOK_ON_STARTUP, WORLDHOOK_ON_SHUTDOWN, WORLDHOOK_ON_UPDATE }) { }

        void OnStartup() override
        {
            UniwowBridge::Bridge::Instance().Start();
        }

        void OnShutdown() override
        {
            UniwowBridge::Bridge::Instance().Stop();
        }

        // On the world thread, after the maps were updated and the commands of the console made.
        void OnUpdate(uint32 /*diff*/) override
        {
            try
            {
                UniwowBridge::Bridge::Instance().Update();
            }
            catch (std::exception const& error)
            {
                LOG_ERROR("module", "uniwow-bridge: {}", error.what());
            }
        }
    };
}

void AddUniwowBridgeScripts()
{
    new BridgeWorld();
}
