import Foundation
import Network

@available(macOS 10.15, iOS 14.0, *)
@MainActor
public final class LibreSyncLocalNetworkPermission {
    private var browser:NWBrowser?
    private var generation=UUID()
    public init(){}
    /// A timeout or empty browse is Unknown. Only actual OS policy-denial
    /// evidence is Denied. Simulator cannot verify physical iOS network privacy.
    public func requestState(timeout:TimeInterval=2,completion:@escaping(LibreSyncPermissionState)->Void){
        browser?.cancel();let token=UUID();generation=token
        let browser=NWBrowser(for:.bonjour(type:"_libresync._tcp",domain:nil),using:.tcp);self.browser=browser
        var finished=false
        let finish:(LibreSyncPermissionState)->Void={state in guard !finished,self.generation==token else{return};finished=true;browser.cancel();completion(state)}
        browser.stateUpdateHandler={state in switch state {
            case .waiting(let error),.failed(let error):if case .dns(let code)=error,code == -65570 {finish(.denied)}
            default:break
        }}
        browser.browseResultsChangedHandler={results,_ in if !results.isEmpty{finish(.allowed)}}
        browser.start(queue:.main)
        DispatchQueue.main.asyncAfter(deadline:.now()+max(0.1,timeout)){finish(.unknown)}
    }
    @available(*,deprecated,message:"Use requestState to distinguish Unknown from permission denial")
    public func request(timeout:TimeInterval=2,completion:@escaping(Bool)->Void){requestState(timeout:timeout){completion($0 == .allowed)}}
}
