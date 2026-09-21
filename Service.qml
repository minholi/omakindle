import QtQuick
import Quickshell
import Quickshell.Io

import "Api.js" as Api

Item {
  id: root

  visible: false
  width: 0
  height: 0

  property var shell: null
  property var manifest: null
  property var pluginRegistry: null

  readonly property string pluginId: manifest && manifest.id
    ? String(manifest.id) : "minholi.kindle"
  readonly property string homeDirectory: Quickshell.env("HOME") || ""
  readonly property string runtimeDirectory: Quickshell.env("XDG_RUNTIME_DIR") || ""
  readonly property string configHome: Quickshell.env("XDG_CONFIG_HOME")
    || (homeDirectory ? homeDirectory + "/.config" : ".config")
  readonly property string pluginDir: manifest && manifest.__sourceDir
    ? String(manifest.__sourceDir) : localSourceDir()
  readonly property string backendPath: homeDirectory + "/.local/lib/omakindle/omakindle-backend"

  readonly property var state: client.lastState
  readonly property var books: state && state.books ? state.books : []
  readonly property var reading: state && state.reading ? state.reading : []
  readonly property var current: reading.length > 0 ? reading[0] : null
  readonly property bool ready: client.lifecycle === "ready"
  readonly property bool refreshing: !!(state && state.refreshing)
  readonly property bool configured: client.lifecycle !== "unconfigured"
  readonly property string lifecycle: client.lifecycle
  readonly property string errorCode: client.errorCode
  readonly property string errorMessage: client.errorMessage
  readonly property string region: state && state.region ? String(state.region) : "us"
  readonly property bool needsDeviceToken: !!(state && state.needsDeviceToken)

  property bool shuttingDown: false
  property bool authRunning: false
  property string authStatus: ""
  property string authBrowser: ""
  property bool authSucceeded: false
  property bool authFailed: false
  property bool authHasToken: false

  signal authFinished(bool ok, bool hasToken, string message)
  signal highlightsUpdated(string asin, var highlights)

  function localSourceDir() {
    var dir = String(Qt.resolvedUrl("."))
    if (dir.indexOf("file://") !== 0)
      return configHome + "/omarchy/plugins/" + pluginId
    dir = dir.substring(7)
    try { dir = decodeURIComponent(dir) } catch (error) {}
    return dir.replace(/\/+$/, "")
  }

  function refresh() {
    return client.sendCommand("refresh", null, null)
  }

  function getHighlights(asin, force, callback) {
    return client.sendCommand("get_highlights", {
      asin: String(asin || ""),
      force: force === true,
      enrich: true
    }, callback)
  }

  function getHighlightText(asin, start, end, callback) {
    return client.sendCommand("get_highlight_text", {
      asin: String(asin || ""),
      start: Number(start),
      end: Number(end)
    }, callback)
  }

  function getRecentHighlights(limit, perBook, force, callback) {
    return client.sendCommand("get_recent_highlights", {
      limit: Number(limit) || 8,
      perBook: Number(perBook) || 4,
      force: force === true
    }, callback)
  }

  function setCredentials(cookies, deviceToken, region, callback) {
    return client.sendCommand("set_credentials", {
      cookies: String(cookies || ""),
      deviceToken: String(deviceToken || ""),
      region: String(region || "us")
    }, callback)
  }

  function setDeviceToken(deviceToken, callback) {
    return client.sendCommand("set_device_token", {
      deviceToken: String(deviceToken || "")
    }, callback)
  }

  function clearCredentials(callback) {
    return client.sendCommand("clear_credentials", null, callback)
  }

  function setRefreshMinutes(minutes) {
    var value = Math.max(5, Math.min(1440, Math.floor(Number(minutes) || 30)))
    return client.sendCommand("set_refresh_minutes", { minutes: value }, null)
  }

  function authorize(region) {
    if (authRunning) return
    if (runtimeDirectory === "") {
      authFailed = true
      authStatus = "XDG_RUNTIME_DIR is not set; refusing to open the local credential socket"
      authFinished(false, false, authStatus)
      return
    }
    authRunning = true
    authSucceeded = false
    authFailed = false
    authHasToken = false
    authStatus = "Opening your browser…"
    authBrowser = ""
    authProcess.command = [
      "uv", "run", "--locked", "--quiet", root.pluginDir + "/scripts/authorize.py",
      "--region", String(region || "us")
    ]
    authProcess.running = true
  }

  function cancelAuthorize() {
    if (authProcess.running) authProcess.running = false
  }

  function handleAuthLine(line) {
    var message = Api.parseJson(line, null)
    if (!message || typeof message !== "object") return
    if (message.browser) authBrowser = String(message.browser)
    if (message.status === "launching")
      authStatus = "Opening " + (authBrowser || "your browser") + "…"
    else if (message.status === "waiting-login")
      authStatus = "Sign in to Amazon in the opened window…"
    else if (message.status === "signed-in")
      authStatus = "Signed in. Capturing the session…"
    else if (message.status === "triggering-token")
      authStatus = "Opening a book to capture the device token…"
    else if (message.status === "saving")
      authStatus = message.hasToken ? "Saving the session…" : "Saving cookies…"
    else if (message.ok === true) {
      authHasToken = message.hasToken === true
      authStatus = authHasToken
        ? "Session saved. Refreshing your library…"
        : "Cookies saved. Paste the device token to finish setup."
    }
    else if (message.ok === false)
      authStatus = String(message.error || "Authorization failed")
  }

  BackendClient {
    id: client
    wanted: true
    onHighlightsReceived: function(asin, highlights) {
      root.highlightsUpdated(asin, highlights)
    }
  }

  Process {
    id: backendProcess
    command: [root.backendPath, "serve"]
    running: root.backendPath !== "" && root.runtimeDirectory !== ""
      && !root.shuttingDown
    onExited: function(code, status) {
      if (!root.shuttingDown) restartTimer.restart()
    }
  }

  Timer {
    id: restartTimer
    interval: 5000
    repeat: false
    onTriggered: if (!root.shuttingDown) backendProcess.running = true
  }

  Process {
    id: authProcess
    command: []
    running: false
    stdout: SplitParser {
      onRead: function(line) { root.handleAuthLine(line) }
    }
    stderr: SplitParser {
      onRead: function(line) { console.log("omakindle authorize:", line) }
    }
    onExited: function(code, status) {
      root.authRunning = false
      var ok = code === 0
      root.authSucceeded = ok && root.authHasToken
      root.authFailed = !ok
      if (!ok && root.authStatus === "")
        root.authStatus = "Authorization failed"
      root.authFinished(ok, root.authHasToken, root.authStatus)
      if (ok) root.refresh()
    }
  }

  Component.onDestruction: root.shuttingDown = true
}
