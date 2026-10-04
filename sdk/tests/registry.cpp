// Tests of the connections of uniwow.hpp over a fake table, run by cargo xtask test-sdk: a
// disconnect or a destroy frees the connected function, and a late call reaches nothing.
#include "uniwow.hpp"

#include <cstdio>
#include <map>

namespace {
uniwow_handle next_handle = 1;
uint64_t next_connection = 1;
struct FakeConnection {
    uniwow_slot slot;
    void *user;
};
std::map<uint64_t, FakeConnection> fake_connections;

uniwow_handle fake_panel(void *, const char *) { return 1000; }
uniwow_handle fake_create(void *, uint32_t, uniwow_handle) { return next_handle++; }
void fake_destroy(void *, uniwow_handle) {}
int32_t fake_add_to(void *, uniwow_handle, uniwow_handle, uint32_t, uint32_t, uint32_t, uint32_t) { return 0; }
int32_t fake_set_text(void *, uniwow_handle, uint32_t, const char *) { return 0; }
uint64_t fake_connect(void *, uniwow_handle, uint32_t, uniwow_slot slot, void *user) {
    fake_connections[next_connection] = {slot, user};
    return next_connection++;
}
void fake_disconnect(void *, uint64_t connection) { fake_connections.erase(connection); }
void fake_log(void *, int32_t, const char *message) { std::printf("log: %s\n", message); }

// Counts the slot functions destroyed.
int destroyed_slots = 0;
struct Tracker {
    ~Tracker() { ++destroyed_slots; }
};

int failures = 0;
void expect(bool ok, const char *what) {
    std::printf("%s %s\n", ok ? "ok  " : "FAIL", what);
    failures += ok ? 0 : 1;
}
} // namespace

int main() {
    static uniwow_api api{};
    api.version = UNIWOW_API_VERSION;
    api.panel = fake_panel;
    api.create = fake_create;
    api.destroy = fake_destroy;
    api.add_to = fake_add_to;
    api.set_text = fake_set_text;
    api.connect = fake_connect;
    api.disconnect = fake_disconnect;
    api.log = fake_log;
    uniwow::start(&api);

    int calls = 0;
    auto counted = [&calls] { return [&calls, tracker = std::make_shared<Tracker>()] { ++calls; (void)tracker; }; };
    uniwow_signal signal{};

    uniwow::Panel panel("main");
    uniwow::VBoxLayout layout;
    panel.setLayout(layout);
    uniwow::PushButton button("a");
    layout.addWidget(button);

    const uint64_t first = button.clicked.connect(counted());
    const FakeConnection kept = fake_connections[first];
    kept.slot(kept.user, &signal);
    expect(calls == 1, "a connected slot is called");
    uniwow::Signal<>::disconnect(first);
    expect(destroyed_slots == 1, "disconnect frees the function");
    kept.slot(kept.user, &signal);
    expect(calls == 1, "a late call after disconnect reaches nothing");

    button.clicked.connect(counted());
    uniwow::PushButton other("b");
    other.clicked.connect(counted());
    layout.destroy();
    expect(destroyed_slots == 2, "destroying a layout frees its widgets' functions, not the others'");

    uniwow::VBoxLayout second;
    uniwow::PushButton inside("c");
    second.addWidget(inside);
    inside.clicked.connect(counted());
    panel.destroy();
    expect(destroyed_slots == 2, "a panel, which the editor keeps, frees nothing");

    other.destroy();
    expect(destroyed_slots == 3, "destroying the sender frees its function");

    uniwow::GroupBox box("g");
    uniwow::VBoxLayout old_layout;
    uniwow::VBoxLayout new_layout;
    box.setLayout(old_layout);
    uniwow::PushButton moved_out("d");
    old_layout.addWidget(moved_out);
    moved_out.clicked.connect(counted());
    box.setLayout(new_layout);
    box.destroy();
    expect(destroyed_slots == 3, "a layout replaced in its group box outlives the group box");

    std::printf("%d failure(s)\n", failures);
    return failures == 0 ? 0 : 1;
}
