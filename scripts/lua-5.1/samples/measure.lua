-- Calls per second from Lua: cube.color runs on the calling thread, cube.paint on the interface
-- thread. Painting the colour the cube already has changes nothing and records no undo entry.
local function rate(count, call)
    local started = os.clock()
    for _ = 1, count do
        call()
    end
    return count / (os.clock() - started)
end

local color = uniwow.call("cube.color").color
local caller = rate(200000, function() uniwow.call("cube.color") end)
print(string.format("calling-thread command: %.0f calls/s", caller))
local interface = rate(20000, function() uniwow.call("cube.paint", {color = color}) end)
print(string.format("interface-thread command: %.0f calls/s", interface))
