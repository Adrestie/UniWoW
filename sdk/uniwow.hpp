// UniWoW for C++: classes named as in Qt over the C interface of uniwow.h. Header only.
//
// Call uniwow::start(api) first in uniwow_module_init. Objects are handles: copying one copies
// the handle, not the object; destroy() destroys it. Signals connect to functions with
// connect(), as in Qt for Python: button.clicked.connect([] { ... }). Slots run on the module's own
// thread, never on the editor's interface thread; an exception a slot lets out is logged.

#pragma once

#include "uniwow.h"

#include <exception>
#include <functional>
#include <initializer_list>
#include <string>
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
        // Kept while the module is loaded, as the connection may be.
        auto *function = new std::function<void(Args...)>(std::move(slot));
        return api().connect(detail::context(), sender_, id_, &Signal::call, function);
    }

    static void disconnect(uint64_t connection) { api().disconnect(detail::context(), connection); }

  private:
    static void call(void *user, const uniwow_signal *signal) {
        try {
            (*static_cast<std::function<void(Args...)> *>(user))(detail::Payload<Args>::from(*signal)...);
        } catch (const std::exception &failure) {
            log(LogLevel::Error, std::string("a slot threw: ") + failure.what());
        } catch (...) {
            log(LogLevel::Error, "a slot threw an exception");
        }
    }

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
        handle_ = 0;
    }

  protected:
    static uniwow_handle make(uint32_t kind, uniwow_handle parent = 0) {
        return api().create(detail::context(), kind, parent);
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
    void addWidget(const Object &widget) const { api().add_to(detail::context(), handle_, widget.handle(), 0, 0, 1, 1); }
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
    void addWidget(const Object &widget, int row, int column, int rowSpan = 1, int columnSpan = 1) const {
        api().add_to(detail::context(), handle_, widget.handle(), uint32_t(row), uint32_t(column), uint32_t(rowSpan),
                     uint32_t(columnSpan));
    }
};

class GroupBox : public Widget {
  public:
    explicit GroupBox(const std::string &title = "") : Widget(make(UNIWOW_GROUP_BOX)) { setTitle(title); }
    void setTitle(const std::string &title) const { setString(UNIWOW_PROPERTY_TITLE, title); }
    void setLayout(const Layout &layout) const { api().add_to(detail::context(), handle_, layout.handle(), 0, 0, 1, 1); }
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
    void setLayout(const Layout &layout) const { api().add_to(detail::context(), handle_, layout.handle(), 0, 0, 1, 1); }
    void show() const { setNumbers(UNIWOW_PROPERTY_VISIBLE, {1.0}); }
    void hide() const { setNumbers(UNIWOW_PROPERTY_VISIBLE, {0.0}); }
    Signal<> rejected{handle_, UNIWOW_SIGNAL_REJECTED};
};

// A dock panel the module declared in uniwow_module_info.
class Panel : public Object {
  public:
    explicit Panel(const std::string &id) : Object(api().panel(detail::context(), id.c_str())) {}
    void setLayout(const Layout &layout) const { api().add_to(detail::context(), handle_, layout.handle(), 0, 0, 1, 1); }
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
        auto *function = new std::function<void(const Painter &, double, double)>(std::move(paint));
        return api().connect(detail::context(), handle_, UNIWOW_SIGNAL_PAINT, &PaintArea::call, function);
    }
    Signal<MouseEvent> mousePressed{handle_, UNIWOW_SIGNAL_MOUSE_PRESS};
    Signal<MouseEvent> mouseMoved{handle_, UNIWOW_SIGNAL_MOUSE_MOVE};
    Signal<MouseEvent> mouseReleased{handle_, UNIWOW_SIGNAL_MOUSE_RELEASE};
    Signal<MouseEvent> wheel{handle_, UNIWOW_SIGNAL_WHEEL};

  private:
    static void call(void *user, const uniwow_signal *signal) {
        try {
            const Painter painter(signal->painter);
            (*static_cast<std::function<void(const Painter &, double, double)> *>(user))(painter, signal->width,
                                                                                       signal->height);
        } catch (...) {
            log(LogLevel::Error, "a paint function threw an exception");
        }
    }
};

} // namespace uniwow
