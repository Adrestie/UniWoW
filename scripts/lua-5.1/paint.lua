-- Paints the cube three times. The whole run is one undo entry: Ctrl+Z restores the colour the
-- cube had before.
uniwow.call("cube.paint", {color = {1, 0, 0}})
uniwow.call("cube.paint", {color = {0, 1, 0}})
uniwow.call("cube.paint", {color = {0, 0, 1}})
print("painted red, green, then blue")
