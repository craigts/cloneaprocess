#import <Foundation/Foundation.h>
#include <dispatch/dispatch.h>
#include <string.h>

#include "xpc_bridge.h"

@protocol EventSinkXPC
- (void)onEvent:(NSDictionary *)event;
@end

@protocol RecorderServiceXPC
- (void)ping:(void (^)(NSDictionary *reply))reply;
- (void)getPermissions:(void (^)(NSDictionary *reply))reply;
- (void)beginCapture:(NSDictionary *)config reply:(void (^)(NSDictionary *reply))reply;
- (void)endCapture:(NSString *)sessionId reply:(void (^)(NSDictionary *reply))reply;
- (void)subscribeEvents:(NSXPCListenerEndpoint *)eventSink reply:(void (^)(NSDictionary *reply))reply;
- (void)unsubscribeEvents:(void (^)(NSDictionary *reply))reply;
@end

struct xpc_client_t {
    void *bridge;
};

static void xpc_invoke_reply_cb(xpc_reply_cb cb, void *user_data, id object) {
    if (cb == NULL) {
        return;
    }

    NSData *data = nil;
    if (object == nil) {
        data = [@"{}" dataUsingEncoding:NSUTF8StringEncoding];
    } else if ([NSJSONSerialization isValidJSONObject:object]) {
        data = [NSJSONSerialization dataWithJSONObject:object options:0 error:nil];
    } else if ([object isKindOfClass:[NSString class]]) {
        data = [(NSString *)object dataUsingEncoding:NSUTF8StringEncoding];
    } else {
        data = [@"{}" dataUsingEncoding:NSUTF8StringEncoding];
    }

    if (data == nil) {
        data = [@"{}" dataUsingEncoding:NSUTF8StringEncoding];
    }

    NSString *json = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    const char *utf8 = json != nil ? json.UTF8String : "{}";
    char *buffer = strdup(utf8 != NULL ? utf8 : "{}");
    cb(buffer, user_data);
    free(buffer);
}

static NSDictionary *xpc_parse_json_dictionary(const char *json) {
    if (json == NULL || strlen(json) == 0) {
        return @{};
    }

    NSData *data = [NSData dataWithBytes:json length:strlen(json)];
    id object = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    if ([object isKindOfClass:[NSDictionary class]]) {
        return object;
    }

    return nil;
}

static NSSet<Class> *xpc_allowed_classes(BOOL includeEndpoint) {
    NSMutableSet<Class> *classes = [NSMutableSet setWithObjects:
        [NSDictionary class],
        [NSArray class],
        [NSString class],
        [NSNumber class],
        [NSNull class],
        nil
    ];
    if (includeEndpoint) {
        [classes addObject:[NSXPCListenerEndpoint class]];
    }
    return classes;
}

@interface XPCEventSink : NSObject <EventSinkXPC>
@property(nonatomic, assign) xpc_event_cb callback;
@property(nonatomic, assign) void *userData;
@end

@implementation XPCEventSink
- (void)onEvent:(NSDictionary *)event {
    if (self.callback == NULL) {
        return;
    }

    NSData *data = [NSJSONSerialization dataWithJSONObject:event options:0 error:nil];
    if (data == nil) {
        return;
    }

    NSString *json = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    const char *utf8 = json != nil ? json.UTF8String : "{}";
    char *buffer = strdup(utf8 != NULL ? utf8 : "{}");
    self.callback(buffer, self.userData);
    free(buffer);
}
@end

@interface XPCEventListenerDelegate : NSObject <NSXPCListenerDelegate>
@property(nonatomic, strong) XPCEventSink *eventSink;
@end

@implementation XPCEventListenerDelegate
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)newConnection {
    NSXPCInterface *interface = [NSXPCInterface interfaceWithProtocol:@protocol(EventSinkXPC)];
    [interface setClasses:xpc_allowed_classes(NO)
              forSelector:@selector(onEvent:)
            argumentIndex:0
                  ofReply:NO];
    newConnection.exportedInterface = interface;
    newConnection.exportedObject = self.eventSink;
    [newConnection resume];
    return YES;
}
@end

@interface XPCClientBridge : NSObject
@property(nonatomic, copy) NSString *serviceName;
@property(nonatomic, assign) xpc_connection_kind_t connectionKind;
@property(nonatomic, assign) xpc_error_cb errorCallback;
@property(nonatomic, assign) void *errorUserData;
@property(nonatomic, strong) NSXPCConnection *connection;
@property(nonatomic, strong) NSXPCListener *eventListener;
@property(nonatomic, strong) XPCEventListenerDelegate *eventListenerDelegate;
@property(nonatomic, strong) XPCEventSink *eventSink;
- (instancetype)initWithServiceName:(NSString *)serviceName connectionKind:(xpc_connection_kind_t)connectionKind errorCallback:(xpc_error_cb)callback userData:(void *)userData;
- (int32_t)connect;
- (void)disconnect;
- (void)emitErrorCode:(int32_t)code message:(NSString *)message;
- (id<RecorderServiceXPC>)recorderProxy;
- (int32_t)subscribeRecorderEvents:(xpc_event_cb)callback userData:(void *)userData;
@end

@implementation XPCClientBridge
- (instancetype)initWithServiceName:(NSString *)serviceName connectionKind:(xpc_connection_kind_t)connectionKind errorCallback:(xpc_error_cb)callback userData:(void *)userData {
    self = [super init];
    if (self == nil) {
        return nil;
    }

    _serviceName = [serviceName copy];
    _connectionKind = connectionKind;
    _errorCallback = callback;
    _errorUserData = userData;
    return self;
}

- (NSXPCInterface *)recorderInterface {
    NSXPCInterface *interface = [NSXPCInterface interfaceWithProtocol:@protocol(RecorderServiceXPC)];
    NSSet<Class> *dictionaryClasses = xpc_allowed_classes(NO);
    NSSet<Class> *configClasses = xpc_allowed_classes(NO);
    NSSet<Class> *eventSinkClasses = xpc_allowed_classes(YES);

    [interface setClasses:dictionaryClasses
              forSelector:@selector(ping:)
            argumentIndex:0
                  ofReply:YES];
    [interface setClasses:dictionaryClasses
              forSelector:@selector(getPermissions:)
            argumentIndex:0
                  ofReply:YES];
    [interface setClasses:configClasses
              forSelector:@selector(beginCapture:reply:)
            argumentIndex:0
                  ofReply:NO];
    [interface setClasses:dictionaryClasses
              forSelector:@selector(beginCapture:reply:)
            argumentIndex:0
                  ofReply:YES];
    [interface setClasses:dictionaryClasses
              forSelector:@selector(endCapture:reply:)
            argumentIndex:0
                  ofReply:YES];
    [interface setClasses:eventSinkClasses
              forSelector:@selector(subscribeEvents:reply:)
            argumentIndex:0
                  ofReply:NO];
    [interface setClasses:dictionaryClasses
              forSelector:@selector(subscribeEvents:reply:)
            argumentIndex:0
                  ofReply:YES];
    [interface setClasses:dictionaryClasses
              forSelector:@selector(unsubscribeEvents:)
            argumentIndex:0
                  ofReply:YES];

    return interface;
}

- (int32_t)connect {
    if (self.connection != nil) {
        return 0;
    }

    if (self.connectionKind == XPC_CONNECTION_KIND_BUNDLED_SERVICE) {
        self.connection = [[NSXPCConnection alloc] initWithServiceName:self.serviceName];
    } else {
        self.connection = [[NSXPCConnection alloc] initWithMachServiceName:self.serviceName options:0];
    }
    self.connection.remoteObjectInterface = [self recorderInterface];

    __weak typeof(self) weakSelf = self;
    self.connection.interruptionHandler = ^{
        [weakSelf emitErrorCode:-2 message:@"XPC connection interrupted"];
    };
    self.connection.invalidationHandler = ^{
        [weakSelf emitErrorCode:-2 message:@"XPC connection invalidated"];
    };
    [self.connection resume];
    return 0;
}

- (void)disconnect {
    [self.connection invalidate];
    self.connection = nil;

    [self.eventListener invalidate];
    self.eventListener = nil;
    self.eventListenerDelegate = nil;
    self.eventSink = nil;
}

- (void)emitErrorCode:(int32_t)code message:(NSString *)message {
    if (self.errorCallback == NULL) {
        return;
    }

    const char *utf8 = message != nil ? message.UTF8String : "";
    char *buffer = strdup(utf8 != NULL ? utf8 : "");
    self.errorCallback(code, buffer, self.errorUserData);
    free(buffer);
}

- (id<RecorderServiceXPC>)recorderProxy {
    if (self.connection == nil) {
        [self emitErrorCode:-2 message:@"XPC client is not connected"];
        return nil;
    }

    __weak typeof(self) weakSelf = self;
    return [self.connection remoteObjectProxyWithErrorHandler:^(NSError *error) {
        [weakSelf emitErrorCode:-3 message:error.localizedDescription ?: @"XPC send failed"];
    }];
}

- (int32_t)subscribeRecorderEvents:(xpc_event_cb)callback userData:(void *)userData {
    if (callback == NULL) {
        return -1;
    }
    id<RecorderServiceXPC> proxy = [self recorderProxy];
    if (proxy == nil) {
        return -2;
    }

    self.eventSink = [XPCEventSink new];
    self.eventSink.callback = callback;
    self.eventSink.userData = userData;

    self.eventListenerDelegate = [XPCEventListenerDelegate new];
    self.eventListenerDelegate.eventSink = self.eventSink;

    self.eventListener = [NSXPCListener anonymousListener];
    self.eventListener.delegate = self.eventListenerDelegate;
    [self.eventListener resume];

    __block int32_t result = -3;
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    [proxy subscribeEvents:self.eventListener.endpoint reply:^(NSDictionary *reply) {
        BOOL ok = [reply[@"ok"] boolValue];
        if (!ok) {
            NSString *message = [NSString stringWithFormat:@"subscribeEvents failed: %@", reply[@"error"] ?: @"unknown error"];
            [self emitErrorCode:-3 message:message];
            result = -3;
        } else {
            result = 0;
        }
        dispatch_semaphore_signal(semaphore);
    }];

    dispatch_time_t timeout = dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC);
    if (dispatch_semaphore_wait(semaphore, timeout) != 0) {
        [self emitErrorCode:-3 message:@"subscribeEvents timed out"];
        result = -3;
    }

    if (result != 0) {
        [self.eventListener invalidate];
        self.eventListener = nil;
        self.eventListenerDelegate = nil;
        self.eventSink = nil;
    }

    return result;
}
@end

static XPCClientBridge *xpc_bridge(xpc_client_t *client) {
    if (client == NULL || client->bridge == NULL) {
        return nil;
    }
    return (__bridge XPCClientBridge *)client->bridge;
}

xpc_client_t *xpc_client_create(
    const char *service_name,
    xpc_connection_kind_t connection_kind,
    xpc_error_cb on_error,
    void *user_data
) {
    if (service_name == NULL) {
        return NULL;
    }

    NSString *name = [NSString stringWithUTF8String:service_name];
    if (name == nil) {
        return NULL;
    }

    xpc_client_t *client = calloc(1, sizeof(xpc_client_t));
    if (client == NULL) {
        return NULL;
    }

    XPCClientBridge *bridge = [[XPCClientBridge alloc] initWithServiceName:name connectionKind:connection_kind errorCallback:on_error userData:user_data];
    client->bridge = (__bridge_retained void *)bridge;
    return client;
}

int32_t xpc_client_connect(xpc_client_t *client) {
    XPCClientBridge *bridge = xpc_bridge(client);
    if (bridge == nil) {
        return -1;
    }
    return [bridge connect];
}

void xpc_client_disconnect(xpc_client_t *client) {
    XPCClientBridge *bridge = xpc_bridge(client);
    [bridge disconnect];
}

void xpc_client_destroy(xpc_client_t *client) {
    if (client == NULL) {
        return;
    }

    XPCClientBridge *bridge = (__bridge_transfer XPCClientBridge *)client->bridge;
    [bridge disconnect];
    client->bridge = NULL;
    free(client);
}

int32_t xpc_recorder_ping(xpc_client_t *client, xpc_reply_cb cb, void *user_data) {
    XPCClientBridge *bridge = xpc_bridge(client);
    id<RecorderServiceXPC> proxy = [bridge recorderProxy];
    if (proxy == nil) {
        return -2;
    }

    [proxy ping:^(NSDictionary *reply) {
        xpc_invoke_reply_cb(cb, user_data, reply);
    }];
    return 0;
}

int32_t xpc_recorder_get_permissions(xpc_client_t *client, xpc_reply_cb cb, void *user_data) {
    XPCClientBridge *bridge = xpc_bridge(client);
    id<RecorderServiceXPC> proxy = [bridge recorderProxy];
    if (proxy == nil) {
        return -2;
    }

    [proxy getPermissions:^(NSDictionary *reply) {
        xpc_invoke_reply_cb(cb, user_data, reply);
    }];
    return 0;
}

int32_t xpc_recorder_begin_capture(xpc_client_t *client, const char *json_config, xpc_reply_cb cb, void *user_data) {
    NSDictionary *config = xpc_parse_json_dictionary(json_config);
    if (config == nil) {
        return -1;
    }

    XPCClientBridge *bridge = xpc_bridge(client);
    id<RecorderServiceXPC> proxy = [bridge recorderProxy];
    if (proxy == nil) {
        return -2;
    }

    [proxy beginCapture:config reply:^(NSDictionary *reply) {
        xpc_invoke_reply_cb(cb, user_data, reply);
    }];
    return 0;
}

int32_t xpc_recorder_end_capture(xpc_client_t *client, const char *session_id, xpc_reply_cb cb, void *user_data) {
    if (session_id == NULL) {
        return -1;
    }

    NSString *sessionId = [NSString stringWithUTF8String:session_id];
    if (sessionId == nil) {
        return -1;
    }

    XPCClientBridge *bridge = xpc_bridge(client);
    id<RecorderServiceXPC> proxy = [bridge recorderProxy];
    if (proxy == nil) {
        return -2;
    }

    [proxy endCapture:sessionId reply:^(NSDictionary *reply) {
        xpc_invoke_reply_cb(cb, user_data, reply);
    }];
    return 0;
}

int32_t xpc_recorder_subscribe_events(xpc_client_t *client, xpc_event_cb cb, void *user_data) {
    XPCClientBridge *bridge = xpc_bridge(client);
    if (bridge == nil) {
        return -1;
    }
    return [bridge subscribeRecorderEvents:cb userData:user_data];
}

int32_t xpc_recorder_unsubscribe_events(xpc_client_t *client, xpc_reply_cb cb, void *user_data) {
    XPCClientBridge *bridge = xpc_bridge(client);
    id<RecorderServiceXPC> proxy = [bridge recorderProxy];
    if (proxy == nil) {
        return -2;
    }

    [proxy unsubscribeEvents:^(NSDictionary *reply) {
        [bridge.eventListener invalidate];
        bridge.eventListener = nil;
        bridge.eventListenerDelegate = nil;
        bridge.eventSink = nil;
        xpc_invoke_reply_cb(cb, user_data, reply);
    }];
    return 0;
}
