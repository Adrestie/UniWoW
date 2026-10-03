-- Paints the cube three times with the colours of palette.lua, another file of this tool. The
-- whole run is one undo entry: Ctrl+Z restores the colour the cube had before.
local palette = require("palette")
uniwow.call("cube.paint", {color = palette.red})
uniwow.call("cube.paint", {color = palette.green})
uniwow.call("cube.paint", {color = palette.blue})
print("painted red, green, then blue")
