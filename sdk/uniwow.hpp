// UniWoW for C++: classes named as in Qt over the C interface of uniwow.h. Header only.
//
// Call uniwow::start(api) first in uniwow_module_init. Objects are handles: copying one copies
// the handle, not the object; destroy() destroys it. Signals connect to functions with
// connect(), as in Qt for Python: button.clicked.connect([] { ... }). Slots run on the module's own
// thread, never on the editor's interface thread; an exception a slot lets out is logged.
// disconnect(), and destroy() on the sender or one of its parents, free the connected function.

#pragma once

#include "uniwow.h"

#include <exception>
#include <functional>
#include <initializer_list>
#include <iterator>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

namespace uniwow {

namespace detail {
inline const uniwow_api *&table() {
    static const uniwow_api *api = nullptr;
    return api;
}
inline void *context() { return table()->context; }
inline void into_string(void *target, const char *text) { *static_cast<std::string *>(target) = text; }
} // namespace detail

// Keeps the table of the C interface; call it first in uniwow_module_init.
inline void start(const uniwow_api *api) { detail::table() = api; }
inline const uniwow_api &api() { return *detail::table(); }

// A colour as uniwow.h takes it.
constexpr uint32_t rgba(uint8_t r, uint8_t g, uint8_t b, uint8_t a = 255) {
    return (uint32_t(r) << 24) | (uint32_t(g) << 16) | (uint32_t(b) << 8) | uint32_t(a);
}

enum class LogLevel : int32_t { Error = 1, Warning = 2, Information = 3, Debug = 4 };
inline void log(LogLevel level, const std::string &message) {
    api().log(detail::context(), int32_t(level), message.c_str());
}

// Calls a command of the editor; true and its JSON result, or false and an error message.
inline std::pair<bool, std::string> call(const std::string &name, const std::string &arguments_json = "{}") {
    std::string answer;
    const int32_t status = api().call(detail::context(), name.c_str(), arguments_json.c_str(), detail::into_string, &answer);
    return {status == 0, answer};
}

// Records a change the module already made, as one undo entry; see record_change in uniwow.h.
inline bool recordChange(const std::string &label, const std::string &undo_json, const std::string &redo_json) {
    return api().record_change(detail::context(), label.c_str(), undo_json.c_str(), redo_json.c_str()) == 0;
}

namespace detail {
// An animatable property declared, kept while the module is loaded.
struct DeclaredProperty {
    std::string name;
    std::string label;
    uint32_t kind;
    double minimum;
    double maximum;
    double initial[3];
    std::function<void(std::vector<double> &)> write;
};
inline std::vector<std::unique_ptr<DeclaredProperty>> &declared_properties() {
    static std::vector<std::unique_ptr<DeclaredProperty>> declared;
    return declared;
}
inline int32_t run_write(void *user, double *values, uint32_t count, uniwow_reply error, void *error_context) {
    try {
        std::vector<double> numbers(values, values + count);
        static_cast<DeclaredProperty *>(user)->write(numbers);
        for (size_t index = 0; index < count && index < numbers.size(); ++index) {
            values[index] = numbers[index];
        }
        return 0;
    } catch (const std::exception &failure) {
        error(error_context, failure.what());
    } catch (...) {
        error(error_context, "the write function threw an exception");
    }
    return 1;
}
} // namespace detail

// An animatable property of the module, <module>/<name>, declared before describeProperties: its
// value is one number, or three for a UNIWOW_VALUE_VECTOR or UNIWOW_VALUE_COLOUR. write receives
// each value written from elsewhere, on the module's thread, and may change it into the value it
// keeps; an exception it lets out makes the module fail. It records nothing: recordChange and the
// undo groups are refused while it runs.
class Property {
  public:
    Property(const std::string &name, const std::string &label, uint32_t kind, double minimum, double maximum,
             std::initializer_list<double> initial, std::function<void(std::vector<double> &)> write)
        : name_(name) {
        auto declared = std::make_unique<detail::DeclaredProperty>(
            detail::DeclaredProperty{name, label, kind, minimum, maximum, {0.0, 0.0, 0.0}, std::move(write)});
        size_t index = 0;
        for (double number : initial) {
            if (index < 3) {
                declared->initial[index++] = number;
            }
        }
        detail::declared_properties().push_back(std::move(declared));
    }
    const std::string &name() const { return name_; }
    // Tells the value the module's property now has; false when refused.
    bool set(std::initializer_list<double> values) const {
        std::vector<double> numbers(values);
        return api().set_property(detail::context(), name_.c_str(), numbers.data(), uint32_t(numbers.size())) == 0;
    }

  private:
    std::string name_;
};

// Fills the properties of info with those declared; call it in uniwow_module_init.
inline void describeProperties(uniwow_module_info *info) {
    auto &declared = detail::declared_properties();
    auto *entries = new uniwow_property[declared.empty() ? 1 : declared.size()]{};
    for (size_t index = 0; index < declared.size(); ++index) {
        const auto &property = *declared[index];
        entries[index] = uniwow_property{property.name.c_str(), property.label.c_str(), property.kind,
                                         property.minimum,      property.maximum,
                                         {property.initial[0], property.initial[1], property.initial[2]},
                                         &detail::run_write,    declared[index].get()};
    }
    info->properties = entries;
    info->property_count = uint32_t(declared.size());
    info->property_size = sizeof(uniwow_property);
}

// The properties of the running modules, as the JSON of properties in uniwow.h.
inline std::string properties() {
    std::string text;
    api().properties(detail::context(), detail::into_string, &text);
    return text;
}

// The numbers of a property; none when refused.
inline std::vector<double> readProperty(const std::string &path) {
    std::vector<double> values(3);
    const uint32_t count = api().read_property(detail::context(), path.c_str(), values.data(), 3);
    values.resize(count < 3 ? count : 3);
    return values;
}

// Writes a property, without the history; false when refused.
inline bool writeProperty(const std::string &path, std::initializer_list<double> values) {
    std::vector<double> numbers(values);
    return api().write_property(detail::context(), path.c_str(), numbers.data(), uint32_t(numbers.size())) == 0;
}

namespace detail {
using Slot = std::function<void(const uniwow_signal &)>;

// What the module connected and created, as the editor has it. The editor hands back a number
// with each signal, not the function: a disconnect or a destroy frees the function, and a call
// under way keeps it until it returns.
struct Registry {
    struct Connected {
        uint64_t connection;
        uniwow_handle sender;
        std::shared_ptr<const Slot> slot;
    };
    std::mutex lock;
    uintptr_t next = 1;
    std::unordered_map<uintptr_t, Connected> connected;
    // The parent of each object, to free what a destroy takes with it.
    std::unordered_map<uniwow_handle, uniwow_handle> parents;
    // The panels, which the editor does not destroy.
    std::unordered_set<uniwow_handle> panels;
};
inline Registry &registry() {
    static Registry instance;
    return instance;
}

inline void run_slot(void *user, const uniwow_signal *signal) {
    std::shared_ptr<const Slot> slot;
    {
        std::lock_guard<std::mutex> guard(registry().lock);
        const auto found = registry().connected.find(reinterpret_cast<uintptr_t>(user));
        if (found == registry().connected.end()) {
            return;
        }
        slot = found->second.slot;
    }
    try {
        (*slot)(*signal);
    } catch (const std::exception &failure) {
        log(LogLevel::Error, std::string("a slot threw: ") + failure.what());
    } catch (...) {
        log(LogLevel::Error, "a slot threw an exception");
    }
}

inline uint64_t connect(uniwow_handle sender, uint32_t signal, Slot slot) {
    auto kept = std::make_shared<const Slot>(std::move(slot));
    Registry &kept_in = registry();
    // Held across connect: a signal sent at once from another thread waits for its function.
    std::lock_guard<std::mutex> guard(kept_in.lock);
    const uintptr_t key = kept_in.next++;
    const uint64_t connection = api().connect(context(), sender, signal, &run_slot, reinterpret_cast<void *>(key));
    if (connection != 0) {
        kept_in.connected.emplace(key, Registry::Connected{connection, sender, std::move(kept)});
    }
    return connection;
}

// Forgets the connections `drop` picks; their functions are freed once the lock is released.
inline void forget(const std::function<bool(const Registry::Connected &)> &drop) {
    std::vector<std::shared_ptr<const Slot>> freed;
    std::lock_guard<std::mutex> guard(registry().lock);
    auto &connected = registry().connected;
    for (auto it = connected.begin(); it != connected.end();) {
        if (drop(it->second)) {
            freed.push_back(std::move(it->second.slot));
            it = connected.erase(it);
        } else {
            ++it;
        }
    }
}

inline void disconnect(uint64_t connection) {
    api().disconnect(context(), connection);
    forget([connection](const Registry::Connected &c) { return c.connection == connection; });
}

// Records that `container` holds `child`; `alone` for the one layout of a panel, group box or dialog.
inline void placed(uniwow_handle container, uniwow_handle child, bool alone) {
    std::lock_guard<std::mutex> guard(registry().lock);
    auto &parents = registry().parents;
    if (alone) {
        for (auto it = parents.begin(); it != parents.end();) {
            it = it->second == container ? parents.erase(it) : std::next(it);
        }
    }
    parents[child] = container;
}

inline uniwow_handle panel(const std::string &id) {
    const uniwow_handle handle = api().panel(context(), id.c_str());
    std::lock_guard<std::mutex> guard(registry().lock);
    registry().panels.insert(handle);
    return handle;
}

// Frees the functions connected to a destroyed object and to its children.
inline void destroyed(uniwow_handle handle) {
    std::unordered_set<uniwow_handle> gone{handle};
    {
        std::lock_guard<std::mutex> guard(registry().lock);
        if (registry().panels.count(handle) != 0) {
            return;
        }
        auto &parents = registry().parents;
        for (bool grew = true; grew;) {
            grew = false;
            for (const auto &[child, parent] : parents) {
                if (gone.count(parent) != 0 && gone.insert(child).second) {
                    grew = true;
                }
            }
        }
        for (const uniwow_handle object : gone) {
            parents.erase(object);
        }
    }
    forget([&gone](const Registry::Connected &c) { return gone.count(c.sender) != 0; });
}
} // namespace detail

// What a signal of a scene item or of the mouse carries.
struct ItemEvent {
    uniwow_handle item;
    double x, y, dx, dy;
    uint32_t button, modifiers;
};
struct MouseEvent {
    double x, y, dx, dy;
    uint32_t button, modifiers;
};

namespace detail {
template <typename T> struct Payload;
template <> struct Payload<bool> {
    static bool from(const uniwow_signal &s) { return s.boolean != 0; }
};
template <> struct Payload<double> {
    static double from(const uniwow_signal &s) { return s.number; }
};
template <> struct Payload<int> {
    static int from(const uniwow_signal &s) { return int(s.integer); }
};
template <> struct Payload<std::string> {
    static std::string from(const uniwow_signal &s) { return s.text != nullptr ? s.text : ""; }
};
template <> struct Payload<ItemEvent> {
    static ItemEvent from(const uniwow_signal &s) { return {s.item, s.x, s.y, s.dx, s.dy, s.button, s.modifiers}; }
};
template <> struct Payload<MouseEvent> {
    static MouseEvent from(const uniwow_signal &s) { return {s.x, s.y, s.dx, s.dy, s.button, s.modifiers}; }
};
} // namespace detail

// A signal of an object; connect() returns the connection, for disconnect().
template <typename... Args> class Signal {
  public:
    Signal(uniwow_handle sender, uint32_t id) : sender_(sender), id_(id) {}

    uint64_t connect(std::function<void(Args...)> slot) const {
        return detail::connect(sender_, id_, [slot = std::move(slot)]([[maybe_unused]] const uniwow_signal &signal) {
            slot(detail::Payload<Args>::from(signal)...);
        });
    }

    static void disconnect(uint64_t connection) { detail::disconnect(connection); }

  private:
    uniwow_handle sender_;
    uint32_t id_;
};

// The base of every interface object.
class Object {
  public:
    explicit Object(uniwow_handle handle = 0) : handle_(handle) {}
    uniwow_handle handle() const { return handle_; }
    explicit operator bool() const { return handle_ != 0; }
    void destroy() {
        api().destroy(detail::context(), handle_);
        detail::destroyed(handle_);
        handle_ = 0;
    }

  protected:
    static uniwow_handle make(uint32_t kind, uniwow_handle parent = 0) {
        const uniwow_handle handle = api().create(detail::context(), kind, parent);
        if (handle != 0 && parent != 0) {
            detail::placed(parent, handle, false);
        }
        return handle;
    }
    // Places a widget or layout in this layout, or sets the one layout of this container.
    void hold(const Object &child, bool alone, uint32_t row = 0, uint32_t column = 0, uint32_t rowSpan = 1,
              uint32_t columnSpan = 1) const {
        if (api().add_to(detail::context(), handle_, child.handle(), row, column, rowSpan, columnSpan) == 0) {
            detail::placed(handle_, child.handle(), alone);
        }
    }
    void setString(uint32_t property, const std::string &text) const {
        api().set_text(detail::context(), handle_, property, text.c_str());
    }
    std::string string(uint32_t property) const {
        std::string text;
        api().text(detail::context(), handle_, property, detail::into_string, &text);
        return text;
    }
    void setNumbers(uint32_t property, std::initializer_list<double> values) const {
        std::vector<double> numbers(values);
        api().set_numbers(detail::context(), handle_, property, numbers.data(), uint32_t(numbers.size()));
    }
    std::vector<double> numbers(uint32_t property) const {
        std::vector<double> values(4);
        const uint32_t count = api().numbers(detail::context(), handle_, property, values.data(), 4);
        values.resize(count < 4 ? count : 4);
        return values;
    }
    double number(uint32_t property) const {
        const auto values = numbers(property);
        return values.empty() ? 0.0 : values[0];
    }

    uniwow_handle handle_;
};

class Widget : public Object {
  public:
    using Object::Object;
    void setEnabled(bool enabled) const { setNumbers(UNIWOW_PROPERTY_ENABLED, {enabled ? 1.0 : 0.0}); }
    void setVisible(bool visible) const { setNumbers(UNIWOW_PROPERTY_VISIBLE, {visible ? 1.0 : 0.0}); }
    void setToolTip(const std::string &text) const { setString(UNIWOW_PROPERTY_TOOL_TIP, text); }
};

class Label : public Widget {
  public:
    explicit Label(const std::string &text = "") : Widget(make(UNIWOW_LABEL)) { setText(text); }
    void setText(const std::string &text) const { setString(UNIWOW_PROPERTY_TEXT, text); }
    std::string text() const { return string(UNIWOW_PROPERTY_TEXT); }
};

class PushButton : public Widget {
  public:
    explicit PushButton(const std::string &text = "") : Widget(make(UNIWOW_PUSH_BUTTON)) { setText(text); }
    void setText(const std::string &text) const { setString(UNIWOW_PROPERTY_TEXT, text); }
    Signal<> clicked{handle_, UNIWOW_SIGNAL_CLICKED};
};

class CheckBox : public Widget {
  public:
    explicit CheckBox(const std::string &text = "") : Widget(make(UNIWOW_CHECK_BOX)) {
        setString(UNIWOW_PROPERTY_TEXT, text);
    }
    void setChecked(bool checked) const { setNumbers(UNIWOW_PROPERTY_CHECKED, {checked ? 1.0 : 0.0}); }
    bool isChecked() const { return number(UNIWOW_PROPERTY_CHECKED) != 0.0; }
    Signal<bool> toggled{handle_, UNIWOW_SIGNAL_TOGGLED};
};

class Slider : public Widget {
  public:
    Slider() : Widget(make(UNIWOW_SLIDER)) {}
    void setRange(double minimum, double maximum) const {
        setNumbers(UNIWOW_PROPERTY_MINIMUM, {minimum});
        setNumbers(UNIWOW_PROPERTY_MAXIMUM, {maximum});
    }
    void setSingleStep(double step) const { setNumbers(UNIWOW_PROPERTY_STEP, {step}); }
    void setValue(double value) const { setNumbers(UNIWOW_PROPERTY_VALUE, {value}); }
    double value() const { return number(UNIWOW_PROPERTY_VALUE); }
    Signal<double> valueChanged{handle_, UNIWOW_SIGNAL_VALUE_CHANGED};
    Signal<double> sliderPressed{handle_, UNIWOW_SIGNAL_SLIDER_PRESSED};
    Signal<double> sliderReleased{handle_, UNIWOW_SIGNAL_SLIDER_RELEASED};
};

class SpinBox : public Widget {
  public:
    SpinBox() : Widget(make(UNIWOW_SPIN_BOX)) {}
    void setRange(double minimum, double maximum) const {
        setNumbers(UNIWOW_PROPERTY_MINIMUM, {minimum});
        setNumbers(UNIWOW_PROPERTY_MAXIMUM, {maximum});
    }
    void setSingleStep(double step) const { setNumbers(UNIWOW_PROPERTY_STEP, {step}); }
    void setDecimals(int decimals) const { setNumbers(UNIWOW_PROPERTY_DECIMALS, {double(decimals)}); }
    void setValue(double value) const { setNumbers(UNIWOW_PROPERTY_VALUE, {value}); }
    double value() const { return number(UNIWOW_PROPERTY_VALUE); }
    Signal<double> valueChanged{handle_, UNIWOW_SIGNAL_VALUE_CHANGED};
    Signal<double> editingFinished{handle_, UNIWOW_SIGNAL_EDITING_FINISHED};
};

class LineEdit : public Widget {
  public:
    explicit LineEdit(const std::string &text = "") : Widget(make(UNIWOW_LINE_EDIT)) { setText(text); }
    void setText(const std::string &text) const { setString(UNIWOW_PROPERTY_TEXT, text); }
    std::string text() const { return string(UNIWOW_PROPERTY_TEXT); }
    void setPlaceholderText(const std::string &text) const { setString(UNIWOW_PROPERTY_PLACEHOLDER, text); }
    Signal<std::string> textChanged{handle_, UNIWOW_SIGNAL_TEXT_CHANGED};
    Signal<std::string> editingFinished{handle_, UNIWOW_SIGNAL_EDITING_FINISHED};
};

class ComboBox : public Widget {
  public:
    ComboBox() : Widget(make(UNIWOW_COMBO_BOX)) {}
    void addItem(const std::string &text) const { api().add_entry(detail::context(), handle_, text.c_str()); }
    void clear() const { api().clear_entries(detail::context(), handle_); }
    int count() const { return int(number(UNIWOW_PROPERTY_COUNT)); }
    void setCurrentIndex(int index) const { setNumbers(UNIWOW_PROPERTY_CURRENT_INDEX, {double(index)}); }
    int currentIndex() const { return int(number(UNIWOW_PROPERTY_CURRENT_INDEX)); }
    Signal<int> currentIndexChanged{handle_, UNIWOW_SIGNAL_CURRENT_INDEX_CHANGED};
};

class Separator : public Widget {
  public:
    Separator() : Widget(make(UNIWOW_SEPARATOR)) {}
};

class Layout : public Object {
  public:
    using Object::Object;
    void addWidget(const Object &widget) const { hold(widget, false); }
    void addLayout(const Object &layout) const { addWidget(layout); }
};

class VBoxLayout : public Layout {
  public:
    VBoxLayout() : Layout(make(UNIWOW_VBOX_LAYOUT)) {}
};

class HBoxLayout : public Layout {
  public:
    HBoxLayout() : Layout(make(UNIWOW_HBOX_LAYOUT)) {}
};

class GridLayout : public Layout {
  public:
    GridLayout() : Layout(make(UNIWOW_GRID_LAYOUT)) {}
    // Rows and columns count from 0, spans from 1, up to 10000.
    void addWidget(const Object &widget, int row, int column, int rowSpan = 1, int columnSpan = 1) const {
        if (row < 0 || column < 0 || rowSpan < 1 || columnSpan < 1) {
            throw std::invalid_argument("a grid cell has a row and a column from 0 and spans from 1");
        }
        hold(widget, false, uint32_t(row), uint32_t(column), uint32_t(rowSpan), uint32_t(columnSpan));
    }
};

class GroupBox : public Widget {
  public:
    explicit GroupBox(const std::string &title = "") : Widget(make(UNIWOW_GROUP_BOX)) { setTitle(title); }
    void setTitle(const std::string &title) const { setString(UNIWOW_PROPERTY_TITLE, title); }
    void setLayout(const Layout &layout) const { hold(layout, true); }
};

// Curves edited by hand, drawn by the module curves: setCurves and curves take the JSON of
// UNIWOW_PROPERTY_CURVES; curvesChanged gives the curves and whether the change is done.
class CurveView : public Widget {
  public:
    CurveView() : Widget(make(UNIWOW_CURVE_VIEW)) {}
    void setCurves(const std::string &json) const { setString(UNIWOW_PROPERTY_CURVES, json); }
    std::string curves() const { return string(UNIWOW_PROPERTY_CURVES); }
    void setMinimumHeight(double height) const { setNumbers(UNIWOW_PROPERTY_MINIMUM_HEIGHT, {height}); }
    Signal<std::string, bool> curvesChanged{handle_, UNIWOW_SIGNAL_CURVES_CHANGED};
};

// A modal window, as QDialog: while it is shown, the rest of the editor cannot be used. It is
// created hidden; the user closing it, with Escape or its close button, hides it and sends rejected.
class Dialog : public Object {
  public:
    explicit Dialog(const std::string &title = "") : Object(make(UNIWOW_DIALOG)) { setTitle(title); }
    void setTitle(const std::string &title) const { setString(UNIWOW_PROPERTY_TITLE, title); }
    void setLayout(const Layout &layout) const { hold(layout, true); }
    void show() const { setNumbers(UNIWOW_PROPERTY_VISIBLE, {1.0}); }
    void hide() const { setNumbers(UNIWOW_PROPERTY_VISIBLE, {0.0}); }
    Signal<> rejected{handle_, UNIWOW_SIGNAL_REJECTED};
};

// A dock panel the module declared in uniwow_module_info.
class Panel : public Object {
  public:
    explicit Panel(const std::string &id) : Object(detail::panel(id)) {}
    void setLayout(const Layout &layout) const { hold(layout, true); }
};

class GraphicsScene : public Object {
  public:
    GraphicsScene() : Object(make(UNIWOW_GRAPHICS_SCENE)) {}
    Signal<ItemEvent> itemPressed{handle_, UNIWOW_SIGNAL_ITEM_PRESSED};
    Signal<ItemEvent> itemMoved{handle_, UNIWOW_SIGNAL_ITEM_MOVED};
    Signal<ItemEvent> itemDoubleClicked{handle_, UNIWOW_SIGNAL_ITEM_DOUBLE_CLICKED};
    Signal<> selectionChanged{handle_, UNIWOW_SIGNAL_SELECTION_CHANGED};
};

// The base of the items of a scene. Their parent is the scene or an item group.
class GraphicsItem : public Object {
  public:
    using Object::Object;
    enum Flag : uint32_t { ItemIsMovableX = 1, ItemIsMovableY = 2, ItemIsMovable = 3 };
    void setPos(double x, double y) const { setNumbers(UNIWOW_PROPERTY_POS, {x, y}); }
    std::pair<double, double> pos() const {
        const auto values = numbers(UNIWOW_PROPERTY_POS);
        return {values.size() > 0 ? values[0] : 0.0, values.size() > 1 ? values[1] : 0.0};
    }
    void setZValue(double z) const { setNumbers(UNIWOW_PROPERTY_Z_VALUE, {z}); }
    void setMovable(uint32_t flags) const { setNumbers(UNIWOW_PROPERTY_MOVABLE, {double(flags)}); }
    void setMoveBounds(double x, double y, double width, double height) const {
        setNumbers(UNIWOW_PROPERTY_MOVE_BOUNDS, {x, y, width, height});
    }
    void setSelectable(bool selectable) const { setNumbers(UNIWOW_PROPERTY_SELECTABLE, {selectable ? 1.0 : 0.0}); }
    void setSelected(bool selected) const { setNumbers(UNIWOW_PROPERTY_SELECTED, {selected ? 1.0 : 0.0}); }
    bool isSelected() const { return number(UNIWOW_PROPERTY_SELECTED) != 0.0; }
    void setVisible(bool visible) const { setNumbers(UNIWOW_PROPERTY_VISIBLE, {visible ? 1.0 : 0.0}); }
    void setToolTip(const std::string &text) const { setString(UNIWOW_PROPERTY_TOOL_TIP, text); }
    void setPen(uint32_t color, double width = 1.0) const {
        setNumbers(UNIWOW_PROPERTY_PEN_COLOR, {double(color)});
        setNumbers(UNIWOW_PROPERTY_PEN_WIDTH, {width});
    }
    void setBrush(uint32_t color) const { setNumbers(UNIWOW_PROPERTY_BRUSH_COLOR, {double(color)}); }
};

class RectItem : public GraphicsItem {
  public:
    RectItem(const Object &parent, double x, double y, double width, double height)
        : GraphicsItem(make(UNIWOW_RECT_ITEM, parent.handle())) {
        setRect(x, y, width, height);
    }
    void setRect(double x, double y, double width, double height) const {
        setNumbers(UNIWOW_PROPERTY_RECT, {x, y, width, height});
    }
    void setRadius(double radius) const { setNumbers(UNIWOW_PROPERTY_RADIUS, {radius}); }
};

class EllipseItem : public GraphicsItem {
  public:
    EllipseItem(const Object &parent, double x, double y, double width, double height)
        : GraphicsItem(make(UNIWOW_ELLIPSE_ITEM, parent.handle())) {
        setNumbers(UNIWOW_PROPERTY_RECT, {x, y, width, height});
    }
};

class LineItem : public GraphicsItem {
  public:
    LineItem(const Object &parent, double x1, double y1, double x2, double y2)
        : GraphicsItem(make(UNIWOW_LINE_ITEM, parent.handle())) {
        setLine(x1, y1, x2, y2);
    }
    void setLine(double x1, double y1, double x2, double y2) const { setNumbers(UNIWOW_PROPERTY_LINE, {x1, y1, x2, y2}); }
};

class TextItem : public GraphicsItem {
  public:
    TextItem(const Object &parent, const std::string &text) : GraphicsItem(make(UNIWOW_TEXT_ITEM, parent.handle())) {
        setText(text);
    }
    void setText(const std::string &text) const { setString(UNIWOW_PROPERTY_TEXT, text); }
    void setFontSize(double size) const { setNumbers(UNIWOW_PROPERTY_FONT_SIZE, {size}); }
    void setColor(uint32_t color) const { setNumbers(UNIWOW_PROPERTY_PEN_COLOR, {double(color)}); }
};

class ItemGroup : public GraphicsItem {
  public:
    explicit ItemGroup(const Object &parent) : GraphicsItem(make(UNIWOW_ITEM_GROUP, parent.handle())) {}
};

class GraphicsView : public Widget {
  public:
    GraphicsView() : Widget(make(UNIWOW_GRAPHICS_VIEW)) {}
    void setScene(const GraphicsScene &scene) const { api().set_scene(detail::context(), handle_, scene.handle()); }
    void setMinimumHeight(double height) const { setNumbers(UNIWOW_PROPERTY_MINIMUM_HEIGHT, {height}); }
    void setScale(double scale) const { setNumbers(UNIWOW_PROPERTY_VIEW_SCALE, {scale}); }
    void centerOn(double x, double y) const { setNumbers(UNIWOW_PROPERTY_VIEW_CENTER, {x, y}); }
};

// Paints a paint area, inside its paint() function.
class Painter {
  public:
    explicit Painter(uniwow_handle painter) : painter_(painter) {}
    void setPen(uint32_t color, double width = 1.0) const { api().set_pen(detail::context(), painter_, color, width); }
    void setBrush(uint32_t color) const { api().set_brush(detail::context(), painter_, color); }
    void drawLine(double x1, double y1, double x2, double y2) const {
        api().draw_line(detail::context(), painter_, x1, y1, x2, y2);
    }
    void drawRect(double x, double y, double width, double height, double radius = 0.0) const {
        api().draw_rect(detail::context(), painter_, x, y, width, height, radius);
    }
    void drawEllipse(double x, double y, double width, double height) const {
        api().draw_ellipse(detail::context(), painter_, x, y, width, height);
    }
    void drawText(double x, double y, const std::string &text, double size = 13.0) const {
        api().draw_text(detail::context(), painter_, x, y, text.c_str(), size);
    }
    void translate(double dx, double dy) const { api().translate(detail::context(), painter_, dx, dy); }
    void scale(double sx, double sy) const { api().scale(detail::context(), painter_, sx, sy); }
    void save() const { api().save(detail::context(), painter_); }
    void restore() const { api().restore(detail::context(), painter_); }

  private:
    uniwow_handle painter_;
};

// A widget the module paints, as a QWidget with its paintEvent.
class PaintArea : public Widget {
  public:
    PaintArea() : Widget(make(UNIWOW_PAINT_AREA)) {}
    void setMinimumHeight(double height) const { setNumbers(UNIWOW_PROPERTY_MINIMUM_HEIGHT, {height}); }
    // Asks for paint() again.
    void update() const { api().update(detail::context(), handle_); }
    // Sets how the area is painted: called with a painter, the width and the height.
    uint64_t paint(std::function<void(const Painter &, double, double)> paint) const {
        return detail::connect(handle_, UNIWOW_SIGNAL_PAINT, [paint = std::move(paint)](const uniwow_signal &signal) {
            paint(Painter(signal.painter), signal.width, signal.height);
        });
    }
    Signal<MouseEvent> mousePressed{handle_, UNIWOW_SIGNAL_MOUSE_PRESS};
    Signal<MouseEvent> mouseMoved{handle_, UNIWOW_SIGNAL_MOUSE_MOVE};
    Signal<MouseEvent> mouseReleased{handle_, UNIWOW_SIGNAL_MOUSE_RELEASE};
    Signal<MouseEvent> wheel{handle_, UNIWOW_SIGNAL_WHEEL};
};

} // namespace uniwow
