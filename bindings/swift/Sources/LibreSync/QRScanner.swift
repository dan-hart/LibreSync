#if os(iOS)
import SwiftUI
import AVFoundation

/// Optional camera permission; denying it leaves the nearby six-digit flow usable.
public struct LibreSyncQRScanner:UIViewControllerRepresentable {
    private let scanned:(String)->Void
    public init(scanned:@escaping(String)->Void){self.scanned=scanned}
    public func makeUIViewController(context:Context)->UIViewController{ScannerController(scanned:scanned)}
    public func updateUIViewController(_ uiViewController:UIViewController,context:Context){}
    public static func dismantleUIViewController(_ uiViewController:UIViewController,coordinator:()){(uiViewController as? ScannerController)?.stop()}
}
private final class ScannerController:UIViewController,AVCaptureMetadataOutputObjectsDelegate {
    private let capture=AVCaptureSession(),worker=DispatchQueue(label:"LibreSync camera")
    private let scanned:(String)->Void
    private var delivered=false
    init(scanned:@escaping(String)->Void){self.scanned=scanned;super.init(nibName:nil,bundle:nil)}
    required init?(coder:NSCoder){nil}
    override func viewDidLoad(){super.viewDidLoad();view.backgroundColor = .black
        AVCaptureDevice.requestAccess(for:.video){[weak self] allowed in guard let self else{return};DispatchQueue.main.async{if allowed{self.configure()}else{let label=UILabel();label.text="Camera unavailable. Close this sheet and use a nearby device’s code.";label.textColor = .white;label.numberOfLines=0;label.frame=self.view.bounds.insetBy(dx:24,dy:24);self.view.addSubview(label)}}}
    }
    private func configure(){guard let camera=AVCaptureDevice.default(for:.video),let input=try? AVCaptureDeviceInput(device:camera),capture.canAddInput(input) else{return};capture.addInput(input);let output=AVCaptureMetadataOutput();guard capture.canAddOutput(output) else{return};capture.addOutput(output);output.setMetadataObjectsDelegate(self,queue:.main);output.metadataObjectTypes=[.qr];let preview=AVCaptureVideoPreviewLayer(session:capture);preview.videoGravity = .resizeAspectFill;preview.frame=view.bounds;view.layer.addSublayer(preview);worker.async{[capture] in capture.startRunning()}}
    func stop(){worker.async{[capture] in capture.stopRunning()}}
    func metadataOutput(_ output:AVCaptureMetadataOutput,didOutput metadataObjects:[AVMetadataObject],from connection:AVCaptureConnection){guard !delivered,let value=(metadataObjects.first as? AVMetadataMachineReadableCodeObject)?.stringValue,value.utf8.count<=16*1024 else{return};delivered=true;stop();scanned(value)}
    deinit{stop()}
}
#endif
