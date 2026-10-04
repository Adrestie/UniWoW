/*
 * UniWoW C interface (rule S1): what a compiled module sees of the editor.
 *
 * Commands, events and settings cross as UTF-8 JSON; the interface objects are reached through
 * typed functions over handles. Every function can be called from any thread (rule T7). Texts
 * handed to a uniwow_reply are valid only during that call: copy them. Nothing returned by the
 * editor has to be freed.
 *
 * No function a module gives the editor (entry point, command handlers, slots, apply_change,
 * reply functions) may let an exception out: one that reaches the editor ends its process.
 *
 * For C++ and C#, sdk/uniwow.hpp and sdk/UniWoW.cs give classes named as in Qt over this table.
 */
#ifndef UNIWOW_H
#define UNIWOW_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define UNIWOW_API_VERSION 3

/* Receives a text produced for the caller: JSON for a result, plain text for an error message.
   A module may pass NULL to the functions of uniwow_api that take one: the text is then ignored.
   The editor never passes NULL to the module's functions. */
typedef void (*uniwow_reply)(void *reply_context, const char *text);

/* An interface object of the module; 0 is no object. */
typedef uint64_t uniwow_handle;

/* <generated kind> from sdk/bindings.toml by cargo xtask bindings */
/* Kinds of interface objects, named as in Qt. */
enum {
    UNIWOW_PANEL = 1,           /* a dock panel; obtained with panel(), never created */
    UNIWOW_LABEL = 2,
    UNIWOW_PUSH_BUTTON = 3,
    UNIWOW_CHECK_BOX = 4,
    UNIWOW_SLIDER = 5,
    UNIWOW_SPIN_BOX = 6,
    UNIWOW_LINE_EDIT = 7,
    UNIWOW_COMBO_BOX = 8,
    UNIWOW_SEPARATOR = 9,
    UNIWOW_GROUP_BOX = 10,
    UNIWOW_VBOX_LAYOUT = 11,
    UNIWOW_HBOX_LAYOUT = 12,
    UNIWOW_GRID_LAYOUT = 13,
    UNIWOW_GRAPHICS_VIEW = 14,
    UNIWOW_GRAPHICS_SCENE = 15,
    UNIWOW_RECT_ITEM = 16,
    UNIWOW_LINE_ITEM = 17,
    UNIWOW_ELLIPSE_ITEM = 18,
    UNIWOW_TEXT_ITEM = 19,
    UNIWOW_ITEM_GROUP = 20,
    UNIWOW_PAINT_AREA = 21,
    UNIWOW_DIALOG = 22,         /* a modal window; created hidden, shown and hidden through VISIBLE */
    UNIWOW_CURVE_VIEW = 23      /* curves edited by hand, drawn by the module curves */
};
/* </generated kind> */

/* <generated property> from sdk/bindings.toml by cargo xtask bindings */
/* Properties. Texts go through set_text; everything else through set_numbers: a flag is 0 or 1,
   a colour is 0xRRGGBBAA, positions and sizes are in points. */
enum {
    UNIWOW_PROPERTY_TEXT = 1,
    UNIWOW_PROPERTY_TOOL_TIP = 2,
    UNIWOW_PROPERTY_ENABLED = 3,
    UNIWOW_PROPERTY_VISIBLE = 4,
    UNIWOW_PROPERTY_CHECKED = 5,
    UNIWOW_PROPERTY_VALUE = 6,           /* slider, spin box; kept within the range */
    UNIWOW_PROPERTY_MINIMUM = 7,
    UNIWOW_PROPERTY_MAXIMUM = 8,
    UNIWOW_PROPERTY_STEP = 9,
    UNIWOW_PROPERTY_DECIMALS = 10,
    UNIWOW_PROPERTY_PLACEHOLDER = 11,    /* line edit, text */
    UNIWOW_PROPERTY_CURRENT_INDEX = 12,  /* combo box */
    UNIWOW_PROPERTY_TITLE = 13,          /* group box, text */
    UNIWOW_PROPERTY_POS = 14,            /* item: x, y in its parent */
    UNIWOW_PROPERTY_RECT = 15,           /* rectangle or ellipse item: x, y, width, height */
    UNIWOW_PROPERTY_LINE = 16,           /* line item: x1, y1, x2, y2 */
    UNIWOW_PROPERTY_PEN_COLOR = 17,
    UNIWOW_PROPERTY_PEN_WIDTH = 18,
    UNIWOW_PROPERTY_BRUSH_COLOR = 19,
    UNIWOW_PROPERTY_RADIUS = 20,         /* rectangle item: corner radius */
    UNIWOW_PROPERTY_Z_VALUE = 21,        /* item: stacking order among its siblings */
    UNIWOW_PROPERTY_MOVABLE = 22,        /* item: 0 no, 1 along x, 2 along y, 3 both */
    UNIWOW_PROPERTY_SELECTABLE = 23,
    UNIWOW_PROPERTY_SELECTED = 24,
    UNIWOW_PROPERTY_MOVE_BOUNDS = 25,    /* movable item: x, y, width, height its position stays in */
    UNIWOW_PROPERTY_FONT_SIZE = 26,      /* text item, 1 to 512, finite */
    UNIWOW_PROPERTY_MINIMUM_HEIGHT = 27, /* graphics view, paint area */
    UNIWOW_PROPERTY_VIEW_SCALE = 28,     /* graphics view: zoom */
    UNIWOW_PROPERTY_VIEW_CENTER = 29,    /* graphics view: x, y of the scene at its centre */
    UNIWOW_PROPERTY_COUNT = 30,          /* read only: entries of a combo box, children otherwise */
    UNIWOW_PROPERTY_CURVES = 31          /* curve view, text: JSON [{label, colour: [r, g, b], visible, keys: [{time,
                                            value, mode, left, right}]}]; keys in time order, each at a time of its
                                            own, numbers within 1e9 */
};
/* </generated property> */

/* <generated signal> from sdk/bindings.toml by cargo xtask bindings */
/* Signals, named as in Qt. Each tells what the user did, never a change the module made. */
enum {
    UNIWOW_SIGNAL_CLICKED = 1,               /* push button */
    UNIWOW_SIGNAL_TOGGLED = 2,               /* check box: boolean */
    UNIWOW_SIGNAL_VALUE_CHANGED = 3,         /* slider, spin box: number */
    UNIWOW_SIGNAL_SLIDER_PRESSED = 4,        /* slider: number */
    UNIWOW_SIGNAL_SLIDER_RELEASED = 5,       /* slider: number; also after a click or a key */
    UNIWOW_SIGNAL_TEXT_CHANGED = 6,          /* line edit: text */
    UNIWOW_SIGNAL_EDITING_FINISHED = 7,      /* line edit: text; spin box: number */
    UNIWOW_SIGNAL_CURRENT_INDEX_CHANGED = 8, /* combo box: integer */
    UNIWOW_SIGNAL_ITEM_PRESSED = 9,          /* scene: item, x, y in the scene, button, modifiers */
    UNIWOW_SIGNAL_ITEM_MOVED = 10,           /* scene: item dropped at x, y, moved by dx, dy */
    UNIWOW_SIGNAL_ITEM_DOUBLE_CLICKED = 11,  /* scene: item, x, y */
    UNIWOW_SIGNAL_SELECTION_CHANGED = 12,    /* scene; read each item's SELECTED */
    UNIWOW_SIGNAL_PAINT = 13,                /* paint area: painter, width, height */
    UNIWOW_SIGNAL_MOUSE_PRESS = 14,          /* paint area: x, y, button, modifiers */
    UNIWOW_SIGNAL_MOUSE_MOVE = 15,           /* paint area, while a button is held: x, y */
    UNIWOW_SIGNAL_MOUSE_RELEASE = 16,        /* paint area: x, y */
    UNIWOW_SIGNAL_WHEEL = 17,                /* paint area: x, y, dx, dy */
    UNIWOW_SIGNAL_REJECTED = 18,             /* dialog: the user closed it, which hid it */
    UNIWOW_SIGNAL_CURVES_CHANGED = 19        /* curve view: text, the curves; boolean, whether the change is done */
};
/* </generated signal> */

/* What a slot receives; the fields its signal does not use are zero. button: 1 left, 2 right,
   3 middle. modifiers: 1 Ctrl, 2 Shift, 4 Alt. text is valid during the call only. */
typedef struct uniwow_signal {
    uniwow_handle sender;
    uint32_t signal;
    uint32_t button;
    uint32_t modifiers;
    int32_t boolean;
    uniwow_handle item;
    int64_t integer;
    double number;
    const char *text;
    double x, y, dx, dy, width, height;
    uniwow_handle painter;
} uniwow_signal;

/* A function connected to a signal. Slots run in order on a thread the editor keeps for the
   module, never on the editor's interface thread. */
typedef void (*uniwow_slot)(void *user, const uniwow_signal *signal);

typedef struct uniwow_api {
    uint32_t version;
    /* Identifies the module: pass it back as the first argument of every function. */
    void *context;

    /* Every command of the editor, as a JSON array of
       {name, owner, description, arguments, result, on_caller}. */
    void (*commands)(void *context, uniwow_reply reply, void *reply_context);

    /* Calls a command and waits for its answer. Returns 0 and replies the JSON result, or returns
       non-zero and replies an error message. A command running on the editor's interface thread
       cannot be awaited from that thread. */
    int32_t (*call)(void *context, const char *name, const char *arguments_json, uniwow_reply reply,
                    void *reply_context);

    void (*publish)(void *context, const char *topic, const char *payload_json);

    /* Receives the events of a topic ("*" for all) from now on. Returns 0 when refused. */
    uint64_t (*subscribe)(void *context, const char *topic);
    /* Waits at most timeout_ms for the next event. Returns 1 and replies
       {"topic", "source", "payload"}; returns 0 when the time passed first; returns -1 and replies
       an error message when the subscription does not exist, was closed, or its module stopped. */
    int32_t (*next_event)(void *context, uint64_t subscription, uint32_t timeout_ms, uniwow_reply reply,
                          void *reply_context);
    void (*unsubscribe)(void *context, uint64_t subscription);

    /* Replies the JSON value of a setting of this module, or null. Each module has settings of its
       own, kept between sessions. */
    void (*setting)(void *context, const char *key, uniwow_reply reply, void *reply_context);
    void (*set_setting)(void *context, const char *key, const char *value_json);

    /* level: 1 error, 2 warning, 3 information, 4 debug. */
    void (*log)(void *context, int32_t level, const char *message);

    /* The commands applied by this module's calls from the calling thread between the two form one
       undo entry. */
    void (*begin_group)(void *context, const char *label);
    void (*end_group)(void *context);

    /* Records a change the module already made to its own state, as one undo entry (or into the
       open group of the calling thread). Undo and redo hand undo_json or redo_json to the module's
       apply_change, on its thread. Returns 0, or non-zero when refused. */
    int32_t (*record_change)(void *context, const char *label, const char *undo_json, const char *redo_json);

    /* --- Interface objects. A refusal returns 0 (handle) or non-zero (status) and is logged. --- */

    /* The panel of that id, declared in uniwow_module_info; it holds one layout. */
    uniwow_handle (*panel)(void *context, const char *panel_id);
    /* Creates an object. An item's parent is a scene or an item group; other objects get none. */
    uniwow_handle (*create)(void *context, uint32_t kind, uniwow_handle parent);
    /* Destroys an object and its children. */
    void (*destroy)(void *context, uniwow_handle object);
    /* Places a widget or layout in a layout (row and column for a grid layout, each up to 10000), or
       sets the layout of a panel, group box or dialog. */
    int32_t (*add_to)(void *context, uniwow_handle container, uniwow_handle child, uint32_t row, uint32_t column,
                      uint32_t row_span, uint32_t column_span);
    int32_t (*set_text)(void *context, uniwow_handle object, uint32_t property, const char *text);
    int32_t (*set_numbers)(void *context, uniwow_handle object, uint32_t property, const double *values,
                           uint32_t count);
    int32_t (*text)(void *context, uniwow_handle object, uint32_t property, uniwow_reply reply, void *reply_context);
    /* Writes at most capacity numbers into values; returns how many the property has. */
    uint32_t (*numbers)(void *context, uniwow_handle object, uint32_t property, double *values, uint32_t capacity);
    int32_t (*add_entry)(void *context, uniwow_handle combo_box, const char *text);
    int32_t (*clear_entries)(void *context, uniwow_handle combo_box);
    int32_t (*set_scene)(void *context, uniwow_handle graphics_view, uniwow_handle scene);
    /* Asks a paint area to be painted again. */
    int32_t (*update)(void *context, uniwow_handle paint_area);
    /* Returns the connection, or 0 when refused. */
    uint64_t (*connect)(void *context, uniwow_handle sender, uint32_t signal, uniwow_slot slot, void *user);
    /* Once disconnect, or destroy on the sender, returns on the module's thread (in a slot, for
       instance), the slot is never called again: its user data may be freed. Called from another
       thread, it does not wait for a call already under way. */
    void (*disconnect)(void *context, uint64_t connection);

    /* --- Painting, as QPainter, inside a slot of UNIWOW_SIGNAL_PAINT, with its painter. --- */
    void (*set_pen)(void *context, uniwow_handle painter, uint32_t rgba, double width);
    void (*set_brush)(void *context, uniwow_handle painter, uint32_t rgba);
    void (*draw_line)(void *context, uniwow_handle painter, double x1, double y1, double x2, double y2);
    void (*draw_rect)(void *context, uniwow_handle painter, double x, double y, double width, double height,
                      double radius);
    void (*draw_ellipse)(void *context, uniwow_handle painter, double x, double y, double width, double height);
    /* The text is drawn from 1 to 512 pixels high; not at all for an infinite size. */
    void (*draw_text)(void *context, uniwow_handle painter, double x, double y, const char *text, double size);
    void (*translate)(void *context, uniwow_handle painter, double dx, double dy);
    void (*scale)(void *context, uniwow_handle painter, double sx, double sy);
    void (*save)(void *context, uniwow_handle painter);
    void (*restore)(void *context, uniwow_handle painter);
} uniwow_api;

/* A command offered by the module, run on the calling thread, possibly several at once. Returns 0
   and replies the JSON result, or returns non-zero and replies an error message. */
typedef int32_t (*uniwow_command_handler)(void *user, const char *arguments_json, uniwow_reply reply,
                                          void *reply_context);

typedef struct uniwow_command {
    const char *name;
    const char *description;
    const char *arguments_schema; /* JSON Schema */
    const char *result_schema;    /* JSON Schema */
    uniwow_command_handler handler;
    void *user;
} uniwow_command;

/* A panel of the module. area: 1 centre, 2 left, 3 right, 4 bottom. */
typedef struct uniwow_panel {
    const char *id;
    const char *title;
    uint32_t area;
} uniwow_panel;

/* Applies a value recorded with record_change, on the module's thread. Returns 0, or replies an
   error message and returns non-zero: the module then fails. */
typedef int32_t (*uniwow_apply_change)(void *user, const char *value_json, uniwow_reply error, void *error_context);

typedef struct uniwow_module_info {
    const char *name;
    const char *version;
    const uniwow_command *commands;
    uint32_t command_count;
    /* Set to UNIWOW_API_VERSION and sizeof(uniwow_command): a module built with another header is
       refused with the reason. */
    uint32_t header_version;
    uint32_t command_size;
    uint32_t panel_count;
    const uniwow_panel *panels;
    uniwow_apply_change apply_change;
    void *user; /* given back to apply_change */
} uniwow_module_info;

/* Exported by every module under the name UNIWOW_MODULE_INIT. Fills info, whose texts must stay
   valid while the module is loaded, and returns 0; or replies an error message and returns
   non-zero. The api table stays valid until the process ends. The module may create its interface
   objects here already; the other functions of the table answer once the module has started. */
typedef int32_t (*uniwow_module_init_fn)(const uniwow_api *api, uniwow_module_info *info, uniwow_reply error,
                                         void *error_context);
#define UNIWOW_MODULE_INIT "uniwow_module_init"

#ifdef __cplusplus
}
#endif

#endif
