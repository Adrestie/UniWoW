-- Loops until stopped, printing each second how long it has run. Start it twice: both run at once.
local started = os.clock()
local next_tick = 1
while true do
    local elapsed = os.clock() - started
    if elapsed >= next_tick then
        print(string.format("running for %d s", next_tick))
        next_tick = next_tick + 1
    end
end
