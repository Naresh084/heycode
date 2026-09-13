// Fixed human dictation helper. Invoked only through heycode-exec.
// `status` never requests permission or opens the microphone.
// `record` emits readiness, waits for stdin EOF, then emits one bounded PCM WAV.
import AVFoundation
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardOutput.write(Data(("ERROR " + message + "\n").utf8))
    exit(1)
}
func permission() -> String {
    switch AVCaptureDevice.authorizationStatus(for: .audio) {
    case .authorized: return "authorized"
    case .notDetermined: return "not_determined"
    case .denied: return "denied"
    case .restricted: return "restricted"
    @unknown default: return "unknown"
    }
}
let action = ProcessInfo.processInfo.environment["HEYCODE_VOICE_ACTION"] ?? "status"
if action == "status" {
    print("microphone_permission=" + permission())
    exit(0)
}
guard action == "record" else { fail("unknown capture action") }
if permission() == "not_determined" {
    // This branch is reached only after the human invokes /voice start.
    let granted = DispatchSemaphore(value: 0)
    AVCaptureDevice.requestAccess(for: .audio) { _ in granted.signal() }
    guard granted.wait(timeout: .now() + 30) == .success else {
        fail("Microphone permission was not answered; retry /voice start when ready.")
    }
}
guard permission() == "authorized" else {
    fail("Microphone permission is " + permission() + ". Enable microphone access for your terminal in System Settings > Privacy & Security > Microphone.")
}

let engine = AVAudioEngine()
let node = engine.inputNode
let format = node.outputFormat(forBus: 0)
guard format.sampleRate >= 8000 && format.sampleRate <= 96000 && format.channelCount > 0 else {
    fail("No supported microphone input is available.")
}
let sampleRate = UInt32(format.sampleRate.rounded())
let maximumSamples = Int(sampleRate) * 60
let lock = NSLock()
var samples = [Int16]()
samples.reserveCapacity(maximumSamples)
var invalid = false
node.installTap(onBus: 0, bufferSize: 2048, format: format) { buffer, _ in
    lock.lock()
    defer { lock.unlock() }
    guard let channels = buffer.floatChannelData else { invalid = true; return }
    let count = min(Int(buffer.frameLength), maximumSamples - samples.count)
    for i in 0..<count {
        let value = channels[0][i]
        guard value.isFinite else { invalid = true; continue }
        samples.append(Int16((max(-1, min(1, value)) * 32767).rounded()))
    }
}
do { try engine.start() } catch { fail("Microphone capture could not start: " + error.localizedDescription) }
FileHandle.standardOutput.write(Data("HEYCODE_VOICE_READY\n".utf8))
// Closing the owned stdin is the stop protocol. The parent also enforces a
// deadline and kills the process tree on cancellation or frontend teardown.
_ = FileHandle.standardInput.readDataToEndOfFile()
engine.stop()
node.removeTap(onBus: 0)
lock.lock()
let captured = samples
let failed = invalid
lock.unlock()
guard !failed && !captured.isEmpty else { fail("No valid microphone samples were captured.") }

var wav = Data()
func word<T: FixedWidthInteger>(_ number: T) {
    var little = number.littleEndian
    withUnsafeBytes(of: &little) { wav.append(contentsOf: $0) }
}
wav.append(Data("RIFF".utf8)); word(UInt32(36 + captured.count * 2))
wav.append(Data("WAVEfmt ".utf8)); word(UInt32(16)); word(UInt16(1)); word(UInt16(1))
word(sampleRate); word(sampleRate * 2); word(UInt16(2)); word(UInt16(16))
wav.append(Data("data".utf8)); word(UInt32(captured.count * 2))
for sample in captured { word(sample) }
FileHandle.standardOutput.write(wav)
