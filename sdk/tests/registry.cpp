// Tests of uniwow.hpp over a fake table, run by cargo xtask test-sdk: a disconnect or a destroy
// frees the connected function, and a late call reaches nothing; properties are declared, written
// and told.
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
// The last text set: on which object, which property, and the text.
uniwow_handle text_object = 0;
uint32_t text_property = 0;
std::string text_set;
int32_t fake_set_text(void *, uniwow_handle object, uint32_t property, const char *text) {
    text_object = object;
    text_property = property;
    text_set = text;
    return 0;
}
uint64_t fake_connect(void *, uniwow_handle, uint32_t, uniwow_slot slot, void *user) {
    fake_connections[next_connection] = {slot, user};
    return next_connection++;
}
void fake_disconnect(void *, uint64_t connection) { fake_connections.erase(connection); }
void fake_log(void *, int32_t, const char *message) { std::printf("log: %s\n", message); }
std::string told_name;
double told_value = 0.0;
int32_t fake_set_property(void *, const char *name, const double *values, uint32_t count) {
    told_name = name;
    told_value = count == 1 ? values[0] : -1.0;
    return 0;
}
void into(void *target, const char *text) { *static_cast<std::string *>(target) = text; }
// The last numbers set: on which object, which property, the first number and all of them; how
// many writes. A sort by the column 99 is refused, as by a table without it.
uniwow_handle numbers_object = 0;
uint32_t numbers_property = 0;
double numbers_value = 0.0;
std::vector<double> numbers_values;
int numbers_calls = 0;
int32_t fake_set_numbers(void *, uniwow_handle object, uint32_t property, const double *values, uint32_t count) {
    numbers_object = object;
    numbers_property = property;
    numbers_value = count > 0 ? values[0] : -1.0;
    numbers_values.assign(values, values + count);
    ++numbers_calls;
    return property == UNIWOW_PROPERTY_SORT_COLUMN && numbers_value == 99.0 ? 1 : 0;
}

// The last cell set and the last ids removed from a table view.
uint64_t cell_row = 0;
uint32_t cell_column = 0;
std::string cell_text;
int cell_calls = 0;
int32_t fake_set_cell(void *, uniwow_handle, uint64_t row, uint32_t column, const char *text) {
    cell_row = row;
    cell_column = column;
    cell_text = text;
    ++cell_calls;
    return 0;
}
std::vector<uint64_t> removed_ids;
int32_t fake_remove_rows(void *, uniwow_handle, const uint64_t *rows, uint32_t count) {
    removed_ids.assign(rows, rows + count);
    return 0;
}

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
    api.set_numbers = fake_set_numbers;
    api.connect = fake_connect;
    api.disconnect = fake_disconnect;
    api.log = fake_log;
    api.set_property = fake_set_property;
    api.set_cell = fake_set_cell;
    api.remove_rows = fake_remove_rows;
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

    uniwow::Property level("level", "Level", UNIWOW_VALUE_NUMBER, 0.0, 10.0, {2.0}, [](std::vector<double> &value) {
        if (value[0] == 7.0) {
            throw std::runtime_error("seven is refused");
        }
        value[0] = 3.0;
    });
    uniwow_module_info info{};
    uniwow::describeProperties(&info);
    const uniwow_property &declared = info.properties[0];
    expect(info.property_count == 1 && info.property_size == sizeof(uniwow_property), "the property is described");
    expect(std::string(declared.name) == "level" && declared.kind == UNIWOW_VALUE_NUMBER &&
               declared.minimum == 0.0 && declared.maximum == 10.0 && declared.initial[0] == 2.0,
           "with its name, kind, range and initial value");
    double written = 3.4;
    std::string error;
    expect(declared.write(declared.user, &written, 1, into, &error) == 0 && written == 3.0,
           "its write function gives back the value it keeps");
    written = 7.0;
    expect(declared.write(declared.user, &written, 1, into, &error) != 0 && error == "seven is refused",
           "an exception of the write function is a failure, with its message");
    expect(level.set({5.0}) && told_name == "level" && told_value == 5.0, "set tells the value");

    uniwow::Sequence sequence;
    uniwow::Player player;
    player.setSequence(sequence);
    expect(numbers_object == player.handle() && numbers_property == UNIWOW_PROPERTY_SEQUENCE &&
               numbers_value == double(sequence.handle()),
           "a player is given its sequence by its handle");
    double time = -1.0;
    const uint64_t timed = player.timeChanged.connect([&time](double frames) { time = frames; });
    uniwow_signal moved{};
    moved.number = 12.5;
    fake_connections[timed].slot(fake_connections[timed].user, &moved);
    expect(time == 12.5, "timeChanged gives the time in frames");

    uniwow::DopesheetView keys;
    keys.setPlayer(player);
    expect(numbers_object == keys.handle() && numbers_property == UNIWOW_PROPERTY_PLAYER &&
               numbers_value == double(player.handle()),
           "a dopesheet view is given its player by its handle");
    std::string tracks;
    bool done = false;
    const uint64_t edited = keys.keysChanged.connect([&](const std::string &json, bool finished) {
        tracks = json;
        done = finished;
    });
    uniwow_signal changed{};
    changed.text = "[]";
    changed.boolean = 1;
    fake_connections[edited].slot(fake_connections[edited].user, &changed);
    expect(tracks == "[]" && done, "keysChanged gives the tracks and whether the change is done");

    uniwow::TreeView tree;
    uint64_t unfolded_item = 0;
    bool unfolded = false;
    const uint64_t folded = tree.itemExpanded.connect([&](uint64_t item, bool expanded) {
        unfolded_item = item;
        unfolded = expanded;
    });
    uniwow_signal toggled{};
    toggled.item = 9007199254740993ULL;
    toggled.boolean = 1;
    fake_connections[folded].slot(fake_connections[folded].user, &toggled);
    expect(unfolded_item == 9007199254740993ULL && unfolded, "itemExpanded gives the item's id whole and whether unfolded");

    uniwow::TableView table;
    expect(table.setCell(7, 2, "x") && cell_row == 7 && cell_column == 2 && cell_text == "x",
           "setCell gives the row's id, the column and the text");
    expect(!table.setCell(7, -1, "y") && cell_calls == 1, "a negative column is refused before the editor");
    expect(table.removeRows({3, 5}) && removed_ids == std::vector<uint64_t>{3, 5}, "removeRows gives the ids");
    uniwow::CellEvent cell{};
    const uint64_t typed = table.cellChanged.connect([&cell](const uniwow::CellEvent &event) { cell = event; });
    uniwow_signal edited_cell{};
    edited_cell.item = 42;
    edited_cell.integer = 1;
    edited_cell.text = "abc";
    fake_connections[typed].slot(fake_connections[typed].user, &edited_cell);
    expect(cell.row == 42 && cell.column == 1 && cell.text == "abc", "cellChanged gives the row, the column and the text");
    int sorted_column = -2;
    bool from_highest = false;
    const uint64_t sorting = table.sortChanged.connect([&](int column, bool descending) {
        sorted_column = column;
        from_highest = descending;
    });
    uniwow_signal sorted{};
    sorted.integer = 3;
    sorted.boolean = 1;
    fake_connections[sorting].slot(fake_connections[sorting].user, &sorted);
    expect(sorted_column == 3 && from_highest, "sortChanged gives the column and whether from the highest");
    const int writes = numbers_calls;
    expect(table.sortByColumn(2, true) && numbers_calls == writes + 1 && numbers_object == table.handle() &&
               numbers_property == UNIWOW_PROPERTY_SORT_COLUMN && numbers_values == std::vector<double>{2.0, 1.0},
           "sortByColumn writes the column and the direction at once");
    expect(!table.sortByColumn(99), "sortByColumn tells a column refused");

    uniwow::PropertyGrid grid;
    grid.setPaths(R"(["cube/colour"])");
    expect(text_object == grid.handle() && text_property == UNIWOW_PROPERTY_PATHS && text_set == R"(["cube/colour"])",
           "a property grid is given the paths of its properties");

    std::printf("%d failure(s)\n", failures);
    return failures == 0 ? 0 : 1;
}
