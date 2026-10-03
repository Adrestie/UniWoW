-- Prints the events of the topic script.demo as they are published, until stopped. Publish one
-- from the console: uniwow.publish("script.demo", {text = "hello"})
local subscription = uniwow.subscribe("script.demo")
print("waiting for script.demo events")
while true do
    local event = uniwow.next_event(subscription)
    local payload = event.payload
    if type(payload) == "table" then
        payload = payload.text
    end
    print(event.source, tostring(payload))
end
