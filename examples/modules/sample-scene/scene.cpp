// Sample compiled module in C++ on the graphics scene: a board of coloured cards. Dragging a card
// is one undo entry, a double click on a card paints the cube in its colour, the wheel zooms and
// the middle or right button scrolls.

#include "uniwow.hpp"

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

namespace {

constexpr double CardWidth = 120.0;
constexpr double CardHeight = 70.0;
constexpr int Columns = 8;

struct Colour {
    double r, g, b;
    const char *name;
};

const Colour Palette[] = {
    {0.80, 0.12, 0.10, "Red"},  {0.15, 0.65, 0.20, "Green"}, {0.15, 0.30, 0.85, "Blue"},
    {1.00, 0.72, 0.18, "Gold"}, {0.55, 0.25, 0.70, "Violet"}, {0.10, 0.60, 0.65, "Teal"},
};

uint32_t rgba(const Colour &c) {
    return uniwow::rgba(uint8_t(c.r * 255), uint8_t(c.g * 255), uint8_t(c.b * 255));
}

struct Card {
    uniwow::ItemGroup group;
    uniwow::RectItem rect;
    uniwow::TextItem label;
    Colour colour;
};

// Everything the module keeps. Commands run on any thread and slots on the module's thread: the
// cards are guarded by `lock`.
struct Board {
    std::mutex lock;
    uniwow::GraphicsScene scene;
    uniwow::GraphicsView view;
    uniwow::Label selection{"Nothing selected."};
    uniwow::PushButton add{"Add card"};
    std::vector<Card> cards;
};

Board *board = nullptr;

// The number following "key": in a flat JSON object; enough for this sample.
double number_after(const char *json, const char *key, double fallback = 0.0) {
    const std::string pattern = std::string("\"") + key + "\"";
    const char *found = std::strstr(json, pattern.c_str());
    if (found == nullptr) {
        return fallback;
    }
    const char *colon = std::strchr(found + pattern.size(), ':');
    return colon != nullptr ? std::strtod(colon + 1, nullptr) : fallback;
}

// The first three numbers of the array following "key".
bool colour_after(const char *json, const char *key, Colour &colour) {
    const std::string pattern = std::string("\"") + key + "\"";
    const char *found = std::strstr(json, pattern.c_str());
    const char *open = found != nullptr ? std::strchr(found, '[') : nullptr;
    if (open == nullptr) {
        return false;
    }
    char *end = nullptr;
    colour.r = std::strtod(open + 1, &end);
    colour.g = std::strtod(std::strchr(end, ',') + 1, &end);
    colour.b = std::strtod(std::strchr(end, ',') + 1, &end);
    colour.name = "Custom";
    return true;
}

std::string format(const char *pattern, double a, double b = 0, double c = 0, double d = 0) {
    char text[256];
    std::snprintf(text, sizeof text, pattern, a, b, c, d);
    return text;
}

// Adds a card; the caller holds the lock.
size_t add_card(double x, double y, const Colour &colour) {
    const size_t id = board->cards.size();
    uniwow::ItemGroup group(board->scene);
    group.setPos(x, y);
    group.setMovable(uniwow::GraphicsItem::ItemIsMovable);
    uniwow::RectItem rect(group, 0, 0, CardWidth, CardHeight);
    rect.setBrush(rgba(colour));
    rect.setPen(uniwow::rgba(20, 20, 20), 1.5);
    rect.setRadius(6);
    rect.setSelectable(true);
    rect.setToolTip(std::string(colour.name) + " card");
    uniwow::TextItem label(group, std::string(colour.name) + " " + std::to_string(id + 1));
    label.setPos(10, 10);
    label.setFontSize(14);
    label.setColor(uniwow::rgba(255, 255, 255));
    board->cards.push_back({group, rect, label, colour});
    return id;
}

// The card whose group or item is `item`, or -1.
int card_of(uniwow_handle item) {
    for (size_t i = 0; i < board->cards.size(); ++i) {
        const Card &card = board->cards[i];
        if (card.group.handle() == item || card.rect.handle() == item || card.label.handle() == item) {
            return int(i);
        }
    }
    return -1;
}

void set_visible(size_t first, size_t last, bool visible) {
    for (size_t i = first; i < last && i < board->cards.size(); ++i) {
        board->cards[i].group.setVisible(visible);
    }
}

// Applies an undo or redo value: {"card", "x", "y"} moves a card; {"first", "last", "visible"}
// shows or hides cards.
int32_t apply_change(void *, const char *value, uniwow_reply error, void *error_context) {
    std::lock_guard<std::mutex> guard(board->lock);
    const double card = number_after(value, "card", -1);
    if (card >= 0) {
        if (size_t(card) >= board->cards.size()) {
            error(error_context, "no such card");
            return 1;
        }
        board->cards[size_t(card)].group.setPos(number_after(value, "x"), number_after(value, "y"));
        return 0;
    }
    set_visible(size_t(number_after(value, "first")), size_t(number_after(value, "last")),
                number_after(value, "visible") != 0);
    return 0;
}

// Adds `count` cards in a grid, as one undo entry when `record`; the caller holds the lock.
void add_cards(size_t count, bool record = true) {
    const size_t first = board->cards.size();
    for (size_t i = 0; i < count; ++i) {
        const size_t n = first + i;
        add_card(double(n % Columns) * (CardWidth + 10), double(n / Columns) * (CardHeight + 10),
                 Palette[n % (sizeof Palette / sizeof Palette[0])]);
    }
    const size_t last = board->cards.size();
    if (!record) {
        return;
    }
    uniwow::recordChange(count == 1 ? "add a card" : "add cards", format("{\"first\":%.0f,\"last\":%.0f,\"visible\":0}", double(first), double(last)),
                         format("{\"first\":%.0f,\"last\":%.0f,\"visible\":1}", double(first), double(last)));
}

int32_t cards_command(void *, const char *, uniwow_reply reply, void *reply_context) {
    std::lock_guard<std::mutex> guard(board->lock);
    std::string answer = "[";
    for (size_t i = 0; i < board->cards.size(); ++i) {
        const auto pos = board->cards[i].group.pos();
        answer += (i > 0 ? "," : "") + format("{\"card\":%.0f,\"x\":%g,\"y\":%g}", double(i), pos.first, pos.second);
    }
    answer += "]";
    reply(reply_context, answer.c_str());
    return 0;
}

int32_t add_card_command(void *, const char *arguments, uniwow_reply reply, void *reply_context) {
    std::lock_guard<std::mutex> guard(board->lock);
    Colour colour = Palette[board->cards.size() % (sizeof Palette / sizeof Palette[0])];
    colour_after(arguments, "color", colour);
    const size_t n = board->cards.size();
    const size_t id = add_card(number_after(arguments, "x", double(n % Columns) * (CardWidth + 10)),
                               number_after(arguments, "y", double(n / Columns) * (CardHeight + 10)), colour);
    uniwow::recordChange("add a card", format("{\"first\":%.0f,\"last\":%.0f,\"visible\":0}", double(id), double(id + 1)),
                         format("{\"first\":%.0f,\"last\":%.0f,\"visible\":1}", double(id), double(id + 1)));
    reply(reply_context, format("{\"card\":%.0f}", double(id)).c_str());
    return 0;
}

int32_t fill_command(void *, const char *arguments, uniwow_reply reply, void *reply_context) {
    const double count = number_after(arguments, "count", 1000);
    if (count < 1 || count > 100000) {
        reply(reply_context, "count must be between 1 and 100000");
        return 1;
    }
    std::lock_guard<std::mutex> guard(board->lock);
    add_cards(size_t(count));
    reply(reply_context, format("{\"cards\":%.0f}", double(board->cards.size())).c_str());
    return 0;
}

const uniwow_command Commands[] = {
    {"scene.cards", "The cards of the board and their positions.", "{\"type\":\"object\"}",
     "{\"type\":\"array\"}", cards_command, nullptr},
    {"scene.add_card", "Adds a card, as one undo entry; x, y and color [r, g, b] are optional.",
     "{\"type\":\"object\",\"properties\":{\"x\":{\"type\":\"number\"},\"y\":{\"type\":\"number\"},"
     "\"color\":{\"type\":\"array\"}}}",
     "{\"type\":\"object\",\"properties\":{\"card\":{\"type\":\"integer\"}}}", add_card_command, nullptr},
    {"scene.fill", "Adds count cards in a grid, as one undo entry, to measure the scene.",
     "{\"type\":\"object\",\"properties\":{\"count\":{\"type\":\"integer\"}}}",
     "{\"type\":\"object\",\"properties\":{\"cards\":{\"type\":\"integer\"}}}", fill_command, nullptr},
};

const uniwow_panel Panels[] = {{"board", "Board", 1}};

void build_panel() {
    uniwow::VBoxLayout layout;
    uniwow::HBoxLayout bar;
    bar.addWidget(board->add);
    bar.addWidget(board->selection);
    layout.addLayout(bar);
    board->view.setScene(board->scene);
    board->view.setMinimumHeight(300);
    // The six cards of the start in the middle of the view.
    board->view.centerOn(3 * (CardWidth + 10) - 5, CardHeight);
    layout.addWidget(board->view);
    uniwow::Panel("board").setLayout(layout);

    board->add.clicked.connect([] {
        std::lock_guard<std::mutex> guard(board->lock);
        add_cards(1);
    });
    board->scene.itemMoved.connect([](uniwow::ItemEvent event) {
        std::lock_guard<std::mutex> guard(board->lock);
        const int card = card_of(event.item);
        if (card < 0) {
            return;
        }
        uniwow::recordChange("move a card",
                             format("{\"card\":%.0f,\"x\":%g,\"y\":%g}", card, event.x - event.dx, event.y - event.dy),
                             format("{\"card\":%.0f,\"x\":%g,\"y\":%g}", card, event.x, event.y));
    });
    board->scene.itemDoubleClicked.connect([](uniwow::ItemEvent event) {
        Colour colour{};
        {
            std::lock_guard<std::mutex> guard(board->lock);
            const int card = card_of(event.item);
            if (card < 0) {
                return;
            }
            colour = board->cards[size_t(card)].colour;
        }
        const auto answer = uniwow::call("cube.paint", format("{\"color\":[%g,%g,%g]}", colour.r, colour.g, colour.b));
        if (!answer.first) {
            uniwow::log(uniwow::LogLevel::Warning, "the cube could not be painted: " + answer.second);
        }
    });
    board->scene.selectionChanged.connect([] {
        std::lock_guard<std::mutex> guard(board->lock);
        int selected = 0;
        std::string names;
        for (const Card &card : board->cards) {
            if (card.rect.isSelected()) {
                names += (selected++ > 0 ? ", " : "") + std::string(card.colour.name);
            }
        }
        board->selection.setText(selected == 0 ? "Nothing selected." : "Selected: " + names);
    });
}

} // namespace

extern "C" __declspec(dllexport) int32_t uniwow_module_init(const uniwow_api *api, uniwow_module_info *info,
                                                            uniwow_reply error, void *error_context) {
    if (api->version != UNIWOW_API_VERSION) {
        error(error_context, "built for another version of uniwow.h");
        return 1;
    }
    try {
        uniwow::start(api);
        board = new Board();
        {
            std::lock_guard<std::mutex> guard(board->lock);
            // The board the module starts with is not a change of the user's.
            add_cards(6, false);
        }
        build_panel();
    } catch (const std::exception &failure) {
        error(error_context, failure.what());
        return 1;
    }
    info->name = "sample-scene";
    info->version = "0.1.0";
    info->commands = Commands;
    info->command_count = sizeof Commands / sizeof Commands[0];
    info->header_version = UNIWOW_API_VERSION;
    info->command_size = sizeof(uniwow_command);
    info->panels = Panels;
    info->panel_count = sizeof Panels / sizeof Panels[0];
    info->apply_change = apply_change;
    info->user = nullptr;
    return 0;
}
