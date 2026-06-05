import Foundation
import Network

@available(macOS 10.15, iOS 14.0, *)
public final class LibreSyncLocalNetworkPermission {
    private var browser: NWBrowser?

    public init() {}

    public func request(timeout: TimeInterval = 2.0, completion: @escaping (Bool) -> Void) {
        let parameters = NWParameters.tcp
        let browser = NWBrowser(for: .bonjour(type: "_libresync._tcp", domain: nil), using: parameters)
        self.browser = browser

        var finished = false
        let finish: (Bool) -> Void = { allowed in
            guard !finished else { return }
            finished = true
            browser.cancel()
            completion(allowed)
        }

        browser.stateUpdateHandler = { state in
            switch state {
            case .ready:
                finish(true)
            case .failed:
                finish(false)
            default:
                break
            }
        }

        browser.start(queue: .main)
        DispatchQueue.main.asyncAfter(deadline: .now() + timeout) {
            finish(true)
        }
    }
}
