-- Fails on line 5: the console shows the message and the line.
print("before the error")
local cube = nil
-- The next line indexes nil.
print(cube.color)
