import QtQuick
import Quickshell
import Quickshell.Io

import "Api.js" as Api

Item {
  id: root

  visible: false
  width: 0
  height: 0

  property bool wanted: false
  readonly property var activeSocket: socketLoader.item
  readonly property bool connected: !!(activeSocket && activeSocket.connected)
  property var lastState: null
  property string lifecycle: "unconfigured"
  property string errorCode: ""
  property string errorMessage: ""
  property int nextId: 1
  property var pending: ({})
  property int reconnectAttempt: 0

  readonly property string socketPath: {
    var runtime = Quickshell.env("XDG_RUNTIME_DIR")
    return String(runtime || "/tmp") + "/omakindle/backend.sock"
  }

  signal stateReceived(var state)
  signal highlightsReceived(string asin, var highlights)

  function resetPending(reason) {
    var waiters = pending
    pending = ({})
    var message = String(reason || "The Kindle backend is unavailable")
    for (var id in waiters) {
      var callback = waiters[id]
      if (typeof callback === "function") callback(false, null, "backend_unavailable", message)
    }
  }

  function sendCommand(name, fields, callback) {
    var socket = activeSocket
    if (!socket || !socket.connected) {
      if (typeof callback === "function")
        callback(false, null, "backend_unavailable", "The Kindle backend is not connected yet")
      return 0
    }
    var id = nextId++
    var payload = { v: 1, id: id, command: String(name || "") }
    for (var key in (fields || {})) payload[key] = fields[key]
    var nextPending = ({})
    for (var existing in pending) nextPending[existing] = pending[existing]
    nextPending[String(id)] = typeof callback === "function" ? callback : null
    pending = nextPending
    socket.write(JSON.stringify(payload) + "\n")
    socket.flush()
    return id
  }

  function applyState(state) {
    if (!state || typeof state !== "object") return
    lastState = state
    lifecycle = String(state.lifecycle || "unconfigured")
    errorCode = String(state.errorCode || "")
    errorMessage = Api.redact(String(state.error || ""))
    stateReceived(state)
  }

  function handleLine(line) {
    var message = Api.parseJson(line, null)
    if (!message || typeof message !== "object") return
    if (message.type === "snapshot" || message.type === "event") {
      if (message.state) applyState(message.state)
      if (message.event === "highlights_changed" && message.highlights)
        highlightsReceived(String(message.asin || ""), message.highlights)
      return
    }
    if (message.type !== "response") return
    var id = String(message.id || "")
    var callback = pending[id]
    var nextPending = ({})
    for (var key in pending) if (key !== id) nextPending[key] = pending[key]
    pending = nextPending
    if (typeof callback !== "function") return
    if (message.ok === true) callback(true, message.result || ({}), "", "")
    else callback(false, null, String(message.errorCode || "backend_error"),
      Api.redact(String(message.error || "Kindle request failed")))
  }

  onWantedChanged: {
    if (wanted) return
    reconnectTimer.stop()
    socketLoader.active = false
    lastState = null
    lifecycle = "unconfigured"
    errorCode = ""
    errorMessage = ""
    reconnectAttempt = 0
    resetPending("The Kindle backend stopped")
  }

  onConnectedChanged: if (connected) reconnectAttempt = 0

  Component {
    id: socketComponent
    Socket {
      path: root.socketPath
      connected: true
      parser: SplitParser {
        splitMarker: "\n"
        onRead: function(line) { root.handleLine(line) }
      }
      onConnectionStateChanged: {
        if (connected) root.sendCommand("hello", null, null)
        else root.lifecycle = "unconfigured"
      }
    }
  }

  Loader {
    id: socketLoader
    active: false
    sourceComponent: socketComponent
  }

  Timer {
    id: reconnectTimer
    interval: Math.min(2000, 200 + root.reconnectAttempt * 150)
    repeat: true
    triggeredOnStart: true
    running: root.wanted && !root.connected
    onTriggered: {
      root.reconnectAttempt = Math.min(12, root.reconnectAttempt + 1)
      socketLoader.active = false
      socketLoader.active = true
    }
  }
}
