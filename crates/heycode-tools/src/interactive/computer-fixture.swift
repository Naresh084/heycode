// Test-owned application. Never targets or inspects another application.
import AppKit

final class Fixture: NSObject {
  var window: NSWindow!
  var field: NSTextField!
  var label: NSTextField!
  func start() {
    window = NSWindow(
      contentRect: NSRect(x: 120, y: 120, width: 420, height: 240),
      styleMask: [.titled, .closable], backing: .buffered, defer: false)
    window.title = "heycode owned computer fixture"
    field = NSTextField(frame: NSRect(x: 24, y: 150, width: 280, height: 28))
    field.setAccessibilityLabel("Fixture name")
    window.contentView!.addSubview(field)
    label = NSTextField(labelWithString: "Waiting")
    label.frame = NSRect(x: 24, y: 50, width: 360, height: 30)
    window.contentView!.addSubview(label)
    let button = NSButton(title: "Fixture greet", target: self, action: #selector(greet))
    button.frame = NSRect(x: 24, y: 95, width: 160, height: 32)
    window.contentView!.addSubview(button)
    window.makeKeyAndOrderFront(nil)
    window.makeFirstResponder(field)
    try?
      "pid=\(ProcessInfo.processInfo.processIdentifier) windows=\(NSApplication.shared.windows.count) visible=\(window.isVisible) number=\(window.windowNumber)"
      .write(toFile: "fixture-state.txt", atomically: true, encoding: .utf8)
  }
  @objc func greet() { label.stringValue = "Hello " + field.stringValue }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let fixture = Fixture()
Timer.scheduledTimer(withTimeInterval: 0.2, repeats: false) { _ in
  fixture.start()
  app.activate(ignoringOtherApps: true)
}
Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { _ in
  if let window = fixture.window {
    try?
      "running=\(app.isRunning) axwindows=\(app.accessibilityWindows()?.count ?? -1) ordered=\(app.orderedWindows.count) visible=\(window.isVisible) text=\(fixture.field.stringValue) label=\(fixture.label.stringValue)"
      .write(toFile: "fixture-state.txt", atomically: true, encoding: .utf8)
  }
}
Timer.scheduledTimer(withTimeInterval: 60, repeats: false) { _ in app.terminate(nil) }
app.run()
