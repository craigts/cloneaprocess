#ifndef XPC_BRIDGE_H
#define XPC_BRIDGE_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>

// Opaque client handle.
typedef struct xpc_client_t xpc_client_t;

// Callback types.
typedef void (*xpc_event_cb)(const char *event_json, void *user_data);
typedef void (*xpc_reply_cb)(const char *reply_json, void *user_data);
typedef void (*xpc_error_cb)(int32_t code, const char *message, void *user_data);

// Return codes.
// 0 = OK
// -1 = invalid args
// -2 = not connected
// -3 = send failed

// Create a client for a specific XPC service.
// service_name examples:
// - "com.yourapp.recorder"
// - "com.yourapp.runner"
xpc_client_t *xpc_client_create(const char *service_name, xpc_error_cb on_error, void *user_data);

// Connect/disconnect lifecycle.
int32_t xpc_client_connect(xpc_client_t *client);
void xpc_client_disconnect(xpc_client_t *client);
void xpc_client_destroy(xpc_client_t *client);

// Recorder service methods.
int32_t xpc_recorder_ping(xpc_client_t *client, xpc_reply_cb cb, void *user_data);
int32_t xpc_recorder_get_permissions(xpc_client_t *client, xpc_reply_cb cb, void *user_data);
int32_t xpc_recorder_begin_capture(xpc_client_t *client, const char *json_config, xpc_reply_cb cb, void *user_data);
int32_t xpc_recorder_end_capture(xpc_client_t *client, const char *session_id, xpc_reply_cb cb, void *user_data);
int32_t xpc_recorder_subscribe_events(xpc_client_t *client, xpc_event_cb cb, void *user_data);
int32_t xpc_recorder_unsubscribe_events(xpc_client_t *client, xpc_reply_cb cb, void *user_data);

// Runner service methods.
int32_t xpc_runner_ping(xpc_client_t *client, xpc_reply_cb cb, void *user_data);
int32_t xpc_runner_run_workflow(xpc_client_t *client, const char *json_request, xpc_reply_cb cb, void *user_data);
int32_t xpc_runner_abort_run(xpc_client_t *client, const char *run_id, xpc_reply_cb cb, void *user_data);
int32_t xpc_runner_subscribe_events(xpc_client_t *client, xpc_event_cb cb, void *user_data);
int32_t xpc_runner_unsubscribe_events(xpc_client_t *client, xpc_reply_cb cb, void *user_data);

#ifdef __cplusplus
}
#endif

#endif
