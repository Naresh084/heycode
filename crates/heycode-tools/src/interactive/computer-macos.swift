import AppKit
import ApplicationServices
import CryptoKit
import Darwin
// Fixed macOS adapter. No script/eval input and no microphone APIs.
import Foundation
import ScreenCaptureKit

struct Failure: Error {
  let code: String
  init(_ code: String) { self.code = code }
}
func emit(_ object: [String: Any]) {
  guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
    data.count <= 12 * 1024 * 1024
  else { return }
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data([10]))
}
func string(_ request: [String: Any], _ key: String) throws -> String {
  guard let value = request[key] as? String else { throw Failure("invalid_request") }
  return value
}
func attr(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
  var value: CFTypeRef?
  guard AXUIElementCopyAttributeValue(element, key as CFString, &value) == .success else {
    return nil
  }
  return value
}
func text(_ element: AXUIElement, _ key: String) -> String {
  String((attr(element, key) as? String ?? "").prefix(256))
}
func point(_ element: AXUIElement) -> CGPoint? {
  guard let raw = attr(element, kAXPositionAttribute), CFGetTypeID(raw) == AXValueGetTypeID() else {
    return nil
  }
  var value = CGPoint.zero
  guard AXValueGetValue(raw as! AXValue, .cgPoint, &value) else { return nil }
  return value
}
func size(_ element: AXUIElement) -> CGSize? {
  guard let raw = attr(element, kAXSizeAttribute), CFGetTypeID(raw) == AXValueGetTypeID() else {
    return nil
  }
  var value = CGSize.zero
  guard AXValueGetValue(raw as! AXValue, .cgSize, &value) else { return nil }
  return value
}
func snapshot(_ application: NSRunningApplication) throws -> ([String: Any], [String: AXUIElement])
{
  let root = AXUIElementCreateApplication(application.processIdentifier)
  AXUIElementSetMessagingTimeout(root, 1.0)
  _ = AXUIElementSetAttributeValue(root, "AXEnhancedUserInterface" as CFString, kCFBooleanTrue)
  var nodes = [[String: Any]]()
  var refs = [String: AXUIElement]()
  var truncated = false
  var seen = [AXUIElement]()
  func walk(_ element: AXUIElement, _ ref: String, _ depth: Int) {
    if depth > 12 || nodes.count >= 160 || seen.contains(where: { CFEqual($0, element) }) {
      truncated = true
      return
    }
    seen.append(element)
    let role = text(element, kAXRoleAttribute)
    let secure =
      role == "AXSecureTextField" || text(element, kAXSubroleAttribute) == "AXSecureTextField"
    var row: [String: Any] = [
      "ref": ref, "role": role, "title": text(element, kAXTitleAttribute),
      "description": text(element, kAXDescriptionAttribute),
      "value": secure ? "[redacted]" : text(element, kAXValueAttribute),
    ]
    if let p = point(element), let s = size(element) {
      row["frame"] = ["x": p.x, "y": p.y, "width": s.width, "height": s.height]
    }
    nodes.append(row)
    refs[ref] = element
    if let children = attr(element, kAXChildrenAttribute) as? [AXUIElement] {
      for (index, child) in children.prefix(160).enumerated() {
        walk(child, ref + "." + String(index), depth + 1)
      }
    }
  }
  // Scope inspection to the target app windows. The system Apple menu can contain
  // unrelated recent files/apps even when it is exposed under this application.
  guard let windows = attr(root, kAXWindowsAttribute) as? [AXUIElement], !windows.isEmpty else {
    throw Failure("target_window_unavailable")
  }
  for (index, window) in windows.prefix(16).enumerated() { walk(window, "0." + String(index), 0) }
  while (try JSONSerialization.data(withJSONObject: nodes, options: [.sortedKeys])).count > 60000 {
    if let removed = nodes.popLast(), let ref = removed["ref"] as? String {
      refs.removeValue(forKey: ref)
    }
    truncated = true
  }
  let data = try JSONSerialization.data(withJSONObject: nodes, options: [.sortedKeys])
  let revision = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
  return (
    [
      "pid": application.processIdentifier, "bundle_id": application.bundleIdentifier ?? "",
      "revision": revision, "nodes": nodes, "truncated": truncated,
    ], refs
  )
}
func app(_ request: [String: Any]) throws -> NSRunningApplication {
  let bundle = try string(request, "bundle_id")
  guard
    let application = NSRunningApplication.runningApplications(withBundleIdentifier: bundle).first(
      where: { !$0.isTerminated && kill($0.processIdentifier, 0) == 0 })
  else { throw Failure("target_not_running") }
  return application
}
func keyFlags(_ request: [String: Any]) throws -> CGEventFlags {
  var flags = CGEventFlags()
  for modifier in request["modifiers"] as? [String] ?? [] {
    switch modifier {
    case "command": flags.insert(.maskCommand)
    case "shift": flags.insert(.maskShift)
    case "option": flags.insert(.maskAlternate)
    case "control": flags.insert(.maskControl)
    default: throw Failure("invalid_modifier")
    }
  }
  return flags
}
@available(macOS 14.0, *)
func liveWindow(_ application: NSRunningApplication) async throws -> SCWindow {
  guard CGPreflightScreenCaptureAccess() else {
    throw Failure("screen_recording_permission_required")
  }
  let content = try await SCShareableContent.excludingDesktopWindows(
    true, onScreenWindowsOnly: false)
  guard
    let window = content.windows.filter({
      $0.owningApplication?.processID == application.processIdentifier && $0.frame.width > 0
        && $0.frame.height > 0 && $0.windowLayer == 0
    }).sorted(by: { left, right in
      if left.isOnScreen != right.isOnScreen { return left.isOnScreen }
      let lt = !(left.title ?? "").isEmpty
      let rt = !(right.title ?? "").isEmpty
      if lt != rt { return lt }
      let la = left.frame.width * left.frame.height
      let ra = right.frame.width * right.frame.height
      return la == ra ? left.windowID < right.windowID : la > ra
    }).first
  else { throw Failure("target_window_unavailable") }
  return window
}
@available(macOS 14.0, *)
func windowView(_ window: SCWindow, _ application: NSRunningApplication) throws -> [String: Any] {
  let frame: [String: Any] = [
    "x": window.frame.minX, "y": window.frame.minY, "width": window.frame.width,
    "height": window.frame.height,
  ]
  let data = try JSONSerialization.data(
    withJSONObject: [
      "frame": frame, "window": window.windowID, "pid": application.processIdentifier,
    ], options: [.sortedKeys])
  return [
    "bundle_id": application.bundleIdentifier ?? "", "frame": frame,
    "revision": SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined(),
    "revision_kind": "window_geometry", "window_id": window.windowID,
    "on_screen": window.isOnScreen,
  ]
}
func perform(_ request: [String: Any]) async throws -> [String: Any] {
  let action = try string(request, "action")
  if action == "status" {
    if request["bundle_id"] != nil {
      let application = try app(request)
      let root = AXUIElementCreateApplication(application.processIdentifier)
      var windows: CFTypeRef?
      let error = AXUIElementCopyAttributeValue(root, kAXWindowsAttribute as CFString, &windows)
      return [
        "target_pid": application.processIdentifier,
        "window_count": (windows as? [AXUIElement])?.count ?? -1, "window_error": error.rawValue,
      ]
    }
    return [
      "platform": "macos", "accessibility": AXIsProcessTrusted(),
      "screen_recording": CGPreflightScreenCaptureAccess(), "microphone": false,
      "setup":
        "Enable the launching app/helper in System Settings > Privacy & Security > Accessibility and Screen & System Audio Recording. This tool does not request permission or record microphone audio.",
    ]
  }
  let application = try app(request)
  if action == "screenshot" {
    guard #available(macOS 14.0, *) else { throw Failure("macos_14_required") }
    let window = try await liveWindow(application)
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let configuration = SCStreamConfiguration()
    let scale = min(1.0, min(1920.0 / window.frame.width, 1080.0 / window.frame.height))
    configuration.width = max(1, Int(window.frame.width * scale))
    configuration.height = max(1, Int(window.frame.height * scale))
    configuration.showsCursor = false
    let image = try await SCScreenshotManager.captureImage(
      contentFilter: filter, configuration: configuration)
    guard let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]),
      png.count <= 8 * 1024 * 1024
    else { throw Failure("image_limit") }
    var result = try windowView(window, application)
    result["png"] = png.base64EncodedString()
    result["width"] = image.width
    result["height"] = image.height
    return result
  }
  guard AXIsProcessTrusted() else { throw Failure("accessibility_permission_required") }
  if action == "inspect" { return try snapshot(application).0 }
  let expected = try string(request, "expected_revision")
  let accessibility = try? snapshot(application)
  if let ref = request["element"] as? String {
    guard let (view, refs) = accessibility, expected == view["revision"] as? String else {
      throw Failure("stale_revision")
    }
    guard let element = refs[ref] else { throw Failure("unknown_element") }
    if action == "click" {
      guard AXUIElementPerformAction(element, kAXPressAction as CFString) == .success else {
        throw Failure("element_not_pressable")
      }
    } else if action == "type" {
      let value = try string(request, "text")
      guard value.utf8.count <= 16384,
        AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFString)
          == .success
      else { throw Failure("element_not_editable") }
    } else {
      throw Failure("invalid_action")
    }
  } else {
    // Pixel/focused-input fallback for apps without accessibility controls. Geometry revisions
    // come from an app-only screenshot, and mouse events must remain inside that same window.
    var targetWindow: SCWindow?
    if action != "key" || accessibility?.0["revision"] as? String != expected {
      guard #available(macOS 14.0, *) else { throw Failure("macos_14_required") }
      let window = try await liveWindow(application)
      guard try windowView(window, application)["revision"] as? String == expected else {
        throw Failure("stale_revision")
      }
      guard window.isOnScreen else { throw Failure("target_window_not_on_screen") }
      targetWindow = window
    }
    guard !application.isTerminated else { throw Failure("target_not_running") }
    if action == "click" {
      guard let x = request["x"] as? Double, let y = request["y"] as? Double,
        let window = targetWindow, window.frame.contains(CGPoint(x: x, y: y))
      else { throw Failure("outside_target_window") }
      guard
        let down = CGEvent(
          mouseEventSource: nil, mouseType: .leftMouseDown,
          mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left),
        let up = CGEvent(
          mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: CGPoint(x: x, y: y),
          mouseButton: .left)
      else { throw Failure("input_unavailable") }
      for event in [down, up] {
        event.setIntegerValueField(
          .mouseEventWindowUnderMousePointer, value: Int64(window.windowID))
        event.setIntegerValueField(
          .mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(window.windowID))
        event.setIntegerValueField(.mouseEventClickState, value: 1)
      }
      down.postToPid(application.processIdentifier)
      try await Task.sleep(nanoseconds: 30_000_000)
      up.postToPid(application.processIdentifier)
    } else if action == "type" {
      let value = try string(request, "text")
      guard value.utf8.count <= 16384 else { throw Failure("invalid_request") }
      let units = Array(value.utf16)
      guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
        let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false)
      else { throw Failure("input_unavailable") }
      units.withUnsafeBufferPointer { buffer in
        down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress)
        up.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress)
      }
      down.postToPid(application.processIdentifier)
      up.postToPid(application.processIdentifier)
    } else if action == "key" {
      let codes: [String: CGKeyCode] = [
        "return": 36, "tab": 48, "escape": 53, "backspace": 51, "delete": 117, "left": 123,
        "right": 124, "down": 125, "up": 126, "a": 0, "c": 8, "v": 9, "x": 7, "z": 6, "s": 1,
      ]
      guard let code = codes[try string(request, "key")] else { throw Failure("unsupported_key") }
      guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true),
        let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false)
      else { throw Failure("input_unavailable") }
      let flags = try keyFlags(request)
      down.flags = flags
      up.flags = flags
      down.postToPid(application.processIdentifier)
      up.postToPid(application.processIdentifier)
    } else {
      throw Failure("invalid_action")
    }
  }
  return ["applied": true, "bundle_id": application.bundleIdentifier ?? "", "inspect_again": true]
}
// Initialize AppKit/WindowServer on the main thread before asynchronous capture.
let helperApplication = NSApplication.shared
helperApplication.setActivationPolicy(.prohibited)
_ = NSScreen.screens
let input = FileHandle.standardInput.readDataToEndOfFile()
guard input.count <= 65536,
  let request = (try? JSONSerialization.jsonObject(with: input)) as? [String: Any]
else {
  emit(["error": "invalid_request"])
  exit(1)
}
Task {
  do {
    emit(try await perform(request))
    exit(0)
  } catch let failure as Failure {
    emit(["error": failure.code])
    exit(0)
  } catch {
    emit(["error": "platform_operation_failed"])
    exit(0)
  }
}
dispatchMain()
