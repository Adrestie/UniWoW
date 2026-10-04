-- Adds three cards to the board of the C++ module sample-scene, in the colours of palette.lua. The
-- whole run is one undo entry: Ctrl+Z removes the three cards.
local palette = require("palette")
for index, name in ipairs({"red", "green", "blue"}) do
    local added = uniwow.call("scene.add_card", {x = (index - 1) * 130, y = 240, color = palette[name]})
    print("added card " .. added.card)
end
