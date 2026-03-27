#if os(macOS)
#if canImport(ApplicationServices)
import ApplicationServices
#endif
import Foundation

@objc protocol RecorderServiceXPC {
    func ping(_ reply: @escaping (String) -> Void)
    func getPermissions(_ reply: @escaping ([String: Bool]) -> Void)
}

final class RecorderServiceImpl: NSObject, RecorderServiceXPC {
    func ping(_ reply: @escaping (String) -> Void) {
        reply("pong")
    }

    func getPermissions(_ reply: @escaping ([String: Bool]) -> Void) {
        let permissions: [String: Bool] = [
            "accessibility": isAccessibilityGranted(),
            "screenRecording": isScreenRecordingGranted(),
        ]

        reply(permissions)
    }

    private func isAccessibilityGranted() -> Bool {
        #if canImport(ApplicationServices)
        return AXIsProcessTrusted()
        #else
        return false
        #endif
    }

    private func isScreenRecordingGranted() -> Bool {
        #if canImport(ApplicationServices)
        return CGPreflightScreenCaptureAccess()
        #else
        return false
        #endif
    }
}

final class RecorderServiceDelegate: NSObject, NSXPCListenerDelegate {
    private let exportedObject = RecorderServiceImpl()

    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection newConnection: NSXPCConnection) -> Bool {
        newConnection.exportedInterface = NSXPCInterface(with: RecorderServiceXPC.self)
        newConnection.exportedObject = exportedObject
        newConnection.resume()
        return true
    }
}

struct RecorderService {
    private let listener: NSXPCListener
    private let delegate = RecorderServiceDelegate()

    init(listener: NSXPCListener = .service()) {
        self.listener = listener
    }

    func run() {
        listener.delegate = delegate
        listener.resume()
        RunLoop.main.run()
    }
}
#else
import Foundation

struct RecorderService {
    func run() {
        print("RecorderService is only available on macOS.")
    }
}
#endif
