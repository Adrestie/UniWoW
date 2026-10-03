// Sample compiled module in C++: one command of its own, and a thread of its own calling the
// editor through the C interface.

#include "../../sdk/uniwow.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>

static const uniwow_api *editor = nullptr;

static void copy_into(void *target, const char *text) { *static_cast<std::string *>(target) = text; }

// The number following "key": in a flat JSON object; enough for this sample.
static double number_after(const char *json, const char *key) {
    const std::string pattern = std::string("\"") + key + "\"";
    const char *found = std::strstr(json, pattern.c_str());
    if (found == nullptr) {
        return 0.0;
    }
    const char *colon = std::strchr(found + pattern.size(), ':');
    return colon != nullptr ? std::strtod(colon + 1, nullptr) : 0.0;
}

static int32_t sum(void *, const char *arguments, uniwow_reply reply, void *reply_context) {
    char result[64];
    std::snprintf(result, sizeof result, "{\"sum\":%g}", number_after(arguments, "a") + number_after(arguments, "b"));
    reply(reply_context, result);
    return 0;
}

static int32_t paint_from_thread(void *, const char *, uniwow_reply reply, void *reply_context) {
    // No exception may reach the editor: starting a thread can throw std::system_error.
    try {
        std::thread([] {
            editor->begin_group(editor->context, "C++ module: gold then green");
            std::string answer;
            const int32_t gold = editor->call(editor->context, "cube.paint", "{\"color\":[1.0,0.72,0.18]}", copy_into, &answer);
            const int32_t green = editor->call(editor->context, "cube.paint", "{\"color\":[0.15,0.65,0.2]}", copy_into, &answer);
            editor->end_group(editor->context);
            const std::string message = (gold == 0 && green == 0 ? "painted from a C++ thread: " : "painting failed: ") + answer;
            editor->log(editor->context, gold == 0 && green == 0 ? 3 : 1, message.c_str());
        }).detach();
    } catch (const std::exception &failure) {
        reply(reply_context, failure.what());
        return 1;
    }
    reply(reply_context, "{\"started\":true}");
    return 0;
}

static const uniwow_command commands[] = {
    {"cpp.sum", "Adds a and b, in C++, on the calling thread.",
     "{\"type\":\"object\",\"properties\":{\"a\":{\"type\":\"number\"},\"b\":{\"type\":\"number\"}}}",
     "{\"type\":\"object\",\"properties\":{\"sum\":{\"type\":\"number\"}}}", sum, nullptr},
    {"cpp.paint_from_thread",
     "Starts a C++ thread that paints the cube gold then green through cube.paint, as one undo entry.",
     "{\"type\":\"object\"}", "{\"type\":\"object\",\"properties\":{\"started\":{\"type\":\"boolean\"}}}",
     paint_from_thread, nullptr},
};

extern "C" __declspec(dllexport) int32_t uniwow_module_init(const uniwow_api *api, uniwow_module_info *info,
                                                            uniwow_reply error, void *error_context) {
    if (api->version != UNIWOW_API_VERSION) {
        error(error_context, "built for another version of uniwow.h");
        return 1;
    }
    editor = api;
    info->name = "sample-cpp";
    info->version = "0.1.0";
    info->commands = commands;
    info->command_count = sizeof commands / sizeof commands[0];
    info->header_version = UNIWOW_API_VERSION;
    info->command_size = sizeof(uniwow_command);
    return 0;
}
