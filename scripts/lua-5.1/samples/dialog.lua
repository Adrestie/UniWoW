-- Opens a window of its own through the module dialogs, then does what the button chosen calls
-- for: here, painting the cube.
local answers = uniwow.subscribe("ui.dialog_answered")
local window = uniwow.call("ui.dialog", {
    title = "Sample",
    text = "Which colour for the cube?",
    buttons = {
        {id = "red", label = "Red"},
        {id = "green", label = "Green"},
        {id = "none", label = "Leave it"},
    },
    escape = "none",
})
local colours = {red = {0.8, 0.12, 0.1}, green = {0.15, 0.65, 0.2}}
while true do
    local event = uniwow.next_event(answers)
    if event.payload.dialog == window.dialog then
        local button = event.payload.button
        print("chosen: " .. button)
        if colours[button] then
            uniwow.call("cube.paint", {color = colours[button]})
        end
        break
    end
end
