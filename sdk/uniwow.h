/*
 * UniWoW C interface (rule S1): what a native module sees of the editor.
 *
 * Every value crosses as UTF-8 JSON. Every function can be called from any thread (rule T7).
 * Texts handed to a uniwow_reply are valid only during that call: copy them. Nothing returned by
 * the editor has to be freed.
 */
#ifndef UNIWOW_H
#define UNIWOW_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define UNIWOW_API_VERSION 1

/* Receives a text produced for the caller: JSON for a result, plain text for an error message. */
typedef void (*uniwow_reply)(void *reply_context, const char *text);

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

    /* Receives the events of a topic ("*" for all) from now on. */
    uint64_t (*subscribe)(void *context, const char *topic);
    /* Waits at most timeout_ms for the next event. Returns 1 and replies
       {"topic", "source", "payload"}, or returns 0. */
    int32_t (*next_event)(void *context, uint64_t subscription, uint32_t timeout_ms, uniwow_reply reply,
                          void *reply_context);
    void (*unsubscribe)(void *context, uint64_t subscription);

    /* Replies the JSON value of a setting of the module, or null. */
    void (*setting)(void *context, const char *key, uniwow_reply reply, void *reply_context);
    void (*set_setting)(void *context, const char *key, const char *value_json);

    /* level: 1 error, 2 warning, 3 information, 4 debug. */
    void (*log)(void *context, int32_t level, const char *message);

    /* The commands applied by this module's calls between the two form one undo entry. */
    void (*begin_group)(void *context, const char *label);
    void (*end_group)(void *context);
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

typedef struct uniwow_module_info {
    const char *name;
    const char *version;
    const uniwow_command *commands;
    uint32_t command_count;
} uniwow_module_info;

/* Exported by every module under the name UNIWOW_MODULE_INIT. Fills info, whose texts must stay
   valid while the module is loaded, and returns 0; or replies an error message and returns
   non-zero. The api table stays valid until the process ends. */
typedef int32_t (*uniwow_module_init_fn)(const uniwow_api *api, uniwow_module_info *info, uniwow_reply error,
                                         void *error_context);
#define UNIWOW_MODULE_INIT "uniwow_module_init"

#ifdef __cplusplus
}
#endif

#endif
