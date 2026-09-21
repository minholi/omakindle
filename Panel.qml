import QtQuick
import QtQuick.Controls
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

import "Api.js" as Api

Panel {
  id: root

  moduleName: "minholi.kindle"
  ipcTarget: "minholi.kindle"
  manageIpc: true

  property var anchorItem: null
  property var hostWidget: null
  property bool openedFromHotkey: false
  readonly property var barIdentity: hostWidget || root

  readonly property var kindle: hostWidget && hostWidget.bar && hostWidget.bar.shell
    ? hostWidget.bar.shell.serviceFor("minholi.kindle") : null
  readonly property var books: kindle && kindle.books ? kindle.books : []
  readonly property var reading: kindle && kindle.reading ? kindle.reading : []
  readonly property bool ready: kindle ? kindle.ready === true : false
  readonly property bool refreshing: kindle ? kindle.refreshing === true : false
  readonly property string errorCode: kindle ? String(kindle.errorCode || "") : ""
  readonly property string errorMessage: kindle ? String(kindle.errorMessage || "") : ""
  readonly property string configuredRegion: kindle ? String(kindle.region || "us") : "us"
  readonly property string progressError: kindle ? String(kindle.progressError || "") : ""

  property int tab: 0
  property string search: ""
  property var highlights: null
  property string highlightsAsin: ""
  property string highlightsTitle: ""
  property string highlightsAuthors: ""
  property bool highlightsLoading: false
  property bool highlightsUpdating: false
  property string highlightsError: ""
  property bool settingsMode: false
  property var copiedItem: null
  property var copyingItem: null

  property bool recentLoading: false
  property int recentScanned: 0
  property var recentHighlights: []
  readonly property int recentPerBook: 4
  readonly property int recentTargetQuotes: 8
  readonly property bool recentMode: highlightsAsin === ""
  readonly property var displayedHighlights: {
    if (!recentMode) return highlights && highlights.items ? highlights.items : []
    return recentHighlights
  }
  readonly property var recentGroups: {
    if (!recentMode) return []
    var groups = []
    var index = ({})
    for (var itemIndex = 0; itemIndex < recentHighlights.length; itemIndex++) {
      var item = recentHighlights[itemIndex]
      var key = String(item.bookAsin || item.bookTitle || "")
      if (index[key] === undefined) {
        index[key] = groups.length
        groups.push({ key: key, title: String(item.bookTitle || ""), items: [] })
      }
      groups[index[key]].items.push(item)
    }
    return groups
  }

  function highlightColor(name) {
    switch (String(name || "").toLowerCase()) {
    case "yellow": return "#e8c547"
    case "orange": return "#e08f4e"
    case "pink": return "#d97a9e"
    case "blue": return "#6ea8d8"
    default: return Color.accent
    }
  }

  component QuoteCard: Rectangle {
    id: card
    required property var entry

    readonly property bool isTruncated: entry && entry.truncated === true

    width: parent ? parent.width : 0
    height: cardColumn.implicitHeight + Style.spacing.lg * 2
    radius: Style.cornerRadius
    color: Util.alpha(root.barForeground, 0.05)
    border.width: 1
    border.color: Util.alpha(root.barForeground, 0.12)

    Rectangle {
      anchors.left: parent.left
      anchors.top: parent.top
      anchors.bottom: parent.bottom
      anchors.topMargin: Style.spacing.sm
      anchors.bottomMargin: Style.spacing.sm
      width: Style.space(3)
      radius: width / 2
      color: root.highlightColor(card.entry ? card.entry.color : "")
    }

    Column {
      id: cardColumn
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(16)
      anchors.rightMargin: Style.space(10)
      spacing: Style.spacing.sm

      Text {
        width: parent.width
        wrapMode: Text.WordWrap
        textFormat: Text.PlainText
        text: card.entry
          ? String(card.entry.text || "") + (card.isTruncated ? "…" : "")
          : ""
        color: root.barForeground
        font.family: root.bar ? root.bar.fontFamily : Style.font.family
        font.pixelSize: Style.font.body
        lineHeight: 1.15
      }

      Text {
        width: parent.width
        visible: card.entry && String(card.entry.note || "") !== ""
        wrapMode: Text.WordWrap
        textFormat: Text.PlainText
        text: card.entry ? String(card.entry.note || "") : ""
        color: Color.muted
        font.family: root.bar ? root.bar.fontFamily : Style.font.family
        font.pixelSize: Style.font.bodySmall
        maximumLineCount: 3
        elide: Text.ElideRight
      }

      Item {
        width: parent.width
        height: Math.max(quoteMeta.implicitHeight, quoteCopy.implicitHeight)

        Text {
          id: quoteMeta
          anchors.left: parent.left
          anchors.right: quoteCopy.left
          anchors.rightMargin: Style.spacing.sm
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: card.entry && card.entry.location
            ? "Location " + String(card.entry.location) : ""
          color: Color.muted
          font.family: root.bar ? root.bar.fontFamily : Style.font.family
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
          maximumLineCount: 1
        }

        PanelActionButton {
          id: quoteCopy
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          iconText: root.copyingItem === card.entry ? "󰦖"
            : (root.copiedItem === card.entry ? "󰄬" : "󰆏")
          tooltipText: root.copyingItem === card.entry ? "Copying…"
            : (root.copiedItem === card.entry ? "Copied" : "Copy quote")
          foreground: root.barForeground
          bordered: true
          size: Style.space(28)
          onClicked: root.copyQuote(card.entry)
        }
      }
    }
  }

  property string cookiesInput: ""
  property string tokenInput: ""
  property string regionInput: ""
  property bool saving: false
  property string saveMessage: ""
  property bool saveFailed: false

  readonly property var filteredBooks: {
    var out = []
    for (var index = 0; index < books.length && out.length < 300; index++)
      if (Api.matchesQuery(books[index], search)) out.push(books[index])
    return out
  }

  function open() {
    openedFromHotkey = false
    controller.show()
    if (kindle) {
      kindle.setRefreshMinutes(Number(root.setting("refreshMinutes", 30)))
      kindle.refresh()
    }
    maybeLoadRecent()
  }

  function openFromHotkey() {
    openedFromHotkey = true
    controller.show()
    if (kindle) {
      kindle.setRefreshMinutes(Number(root.setting("refreshMinutes", 30)))
      kindle.refresh()
    }
    maybeLoadRecent()
  }

  function maybeLoadRecent() {
    if (tab === 2 && recentMode && recentHighlights.length === 0 && !recentLoading)
      loadRecentHighlights()
  }

  function close() {
    controller.hide()
  }

  function toggle() {
    if (opened) close()
    else openFromHotkey()
  }

  function refresh() {
    if (!kindle) return
    if (tab === 2 && !settingsMode) {
      if (recentMode) loadRecentHighlights(true)
      else reloadHighlights()
    }
    kindle.refresh()
  }

  function reloadHighlights() {
    if (!kindle || highlightsAsin === "") return
    highlightsLoading = true
    highlightsUpdating = false
    highlightsError = ""
    kindle.getHighlights(highlightsAsin, true, function(ok, result, code, message) {
      highlightsLoading = false
      if (ok && result && result.highlights) highlights = result.highlights
      else highlightsError = message || "Could not load highlights"
    })
  }

  function openBook(book) {
    if (book && book.webReaderUrl) Qt.openUrlExternally(String(book.webReaderUrl))
  }

  function loadHighlights(book) {
    if (!kindle || !book) return
    recentLoading = false
    tab = 2
    highlightsAsin = String(book.asin || "")
    highlightsTitle = String(book.title || "")
    highlightsAuthors = (book.authors || []).join(", ")
    highlights = null
    highlightsError = ""
    highlightsLoading = true
    highlightsUpdating = false
    kindle.getHighlights(book.asin, false, function(ok, result, code, message) {
      highlightsLoading = false
      if (ok && result && result.highlights) {
        highlights = result.highlights
        highlightsUpdating = result.stale === true
      } else {
        highlightsError = message || "Could not load highlights"
      }
    })
  }

  function loadRecentHighlights(force) {
    if (!kindle) return
    highlightsAsin = ""
    highlightsTitle = ""
    highlights = null
    highlightsError = ""
    recentHighlights = []
    recentScanned = 0
    recentLoading = true
    kindle.getRecentHighlights(recentTargetQuotes, recentPerBook, force === true,
      function(ok, result, code, message) {
        recentLoading = false
        if (ok && result) {
          recentHighlights = result.items || []
          recentScanned = Number(result.scanned || 0)
        }
      })
  }

  function copyText(text) {
    if (text) Quickshell.execDetached(["wl-copy", String(text)])
  }

  function finishCopy(item, quote) {
    var author = String(item.bookAuthors || root.highlightsAuthors || "")
    var title = String(item.bookTitle || root.highlightsTitle || "")
    var citation = author !== "" && title !== "" ? author + ", " + title
      : (author !== "" ? author : title)
    var payload = quote
    if (citation !== "") payload += "\n\n— " + citation
    copyText(payload)
    copiedItem = item
    copyingItem = null
    copiedTimer.restart()
  }

  function copyQuote(item) {
    if (!item || copyingItem === item) return
    var preview = String(item.text || "")
    var asin = String(item.bookAsin || root.highlightsAsin || "")
    if (item.verified !== true && asin !== "" && root.kindle
        && item.start !== undefined && item.end !== undefined) {
      copyingItem = item
      root.kindle.getHighlightText(asin, item.start, item.end,
        function(ok, result, code, message) {
          if (ok && result && result.text) root.finishCopy(item, String(result.text))
          else root.finishCopy(item, preview + (item.truncated === true ? "…" : ""))
        })
      return
    }
    root.finishCopy(item, preview)
  }

  function saveSetting(key, value) {
    var entry = { id: root.moduleName }
    for (var existing in root.settings) if (existing !== "id") entry[existing] = root.settings[existing]
    entry[key] = value
    root.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function")
      root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  function regionSetting() {
    return String(root.setting("region", "us"))
  }

  function saveCredentials() {
    if (!kindle) return
    saving = true
    saveMessage = ""
    saveFailed = false
    var finishSetup = kindle.needsDeviceToken === true
      && root.cookiesInput === "" && root.tokenInput !== ""
    var handler = function(ok, result, code, message) {
      saving = false
      if (ok) {
        saveFailed = false
        saveMessage = finishSetup
          ? "Device token saved. Refreshing…"
          : "Session saved. Library is refreshing."
        cookiesInput = ""
        tokenInput = ""
        if (!finishSetup) root.saveSetting("region", Api.normalizeRegion(regionInput))
      } else {
        saveFailed = true
        saveMessage = message || "Could not save the session"
      }
    }
    if (finishSetup) kindle.setDeviceToken(tokenInput, handler)
    else kindle.setCredentials(cookiesInput, tokenInput, regionInput, handler)
  }

  function clearSession() {
    if (!kindle) return
    saving = true
    saveMessage = ""
    saveFailed = false
    kindle.clearCredentials(function(ok, result, code, message) {
      saving = false
      if (ok) {
        saveMessage = "Session cleared"
        saveFailed = false
      } else {
        saveFailed = true
        saveMessage = message || "Could not clear the session"
      }
    })
  }

  Component.onCompleted: {
    regionInput = regionSetting()
  }
  onConfiguredRegionChanged: if (!saving) regionInput = configuredRegion
  onTabChanged: body.contentY = 0
  onSettingsModeChanged: body.contentY = 0
  onReadingChanged: if (!saving) Qt.callLater(maybeLoadRecent)

  Connections {
    target: root.kindle
    function onHighlightsUpdated(asin, highlights) {
      if (!highlights || !highlights.items) return
      var items = highlights.items
      if (!root.recentMode && String(asin) === root.highlightsAsin) {
        root.highlights = highlights
        root.highlightsUpdating = false
        root.highlightsLoading = false
        root.highlightsError = ""
      }
      if (root.recentHighlights.length > 0) {
        var next = []
        var changed = false
        for (var index = 0; index < root.recentHighlights.length; index++) {
          var item = root.recentHighlights[index]
          var match = null
          if (String(item.bookAsin || "") === String(asin)) {
            for (var itemIndex = 0; itemIndex < items.length; itemIndex++) {
              if (items[itemIndex].start === item.start && items[itemIndex].end === item.end) {
                match = items[itemIndex]
                break
              }
            }
          }
          if (match && match.text !== item.text) {
            var updated = {}
            for (var key in item) updated[key] = item[key]
            updated.text = match.text
            updated.truncated = match.truncated
            updated.verified = match.verified
            next.push(updated)
            changed = true
          } else {
            next.push(item)
          }
        }
        if (changed) root.recentHighlights = next
      }
    }
  }

  Timer {
    id: copiedTimer
    interval: 1600
    repeat: false
    onTriggered: root.copiedItem = null
  }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    centerOnBar: true
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(560))
    contentHeight: panel.fittedContentHeight(shell.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: searchField.activeFocus || cookiesField.activeFocus
        || tokenField.activeFocus || regionField.activeFocus
      onCloseRequested: root.close()
      onTabRequested: function(direction) {
        if (root.bar && typeof root.bar.switchPanelFrom === "function")
          root.bar.switchPanelFrom(root.barIdentity, direction)
      }
      onMoveRequested: function(dx, dy) {
        if (dy !== 0) body.scrollBy(dy)
      }

      Item {
        id: shell
        anchors.fill: parent
        readonly property int gap: Style.space(12)
        implicitHeight: tabBar.height + divider.height
          + bodyColumn.implicitHeight + gap * 2

        Item {
          id: tabBar
          anchors.top: parent.top
          anchors.left: parent.left
          anchors.right: parent.right
          height: Math.max(tabRow.implicitHeight, refreshButton.implicitHeight)

          Row {
            id: brandRow
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(6)

            Item {
              anchors.verticalCenter: parent.verticalCenter
              width: Style.space(15)
              height: Style.space(17)

              Image {
                id: brandImage
                anchors.fill: parent
                source: Qt.resolvedUrl("assets/omakindle.svg")
                sourceSize.width: 32
                sourceSize.height: 32
                fillMode: Image.PreserveAspectFit
                visible: false
              }

              MultiEffect {
                anchors.fill: brandImage
                source: brandImage
                colorizationColor: root.barForeground
                colorization: 1.0
              }
            }

            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: "OmaKindle"
              color: root.barForeground
              font.family: root.bar ? root.bar.fontFamily : Style.font.family
              font.pixelSize: Style.font.body
              font.bold: true
            }
          }

          Row {
            id: tabRow
            anchors.left: brandRow.right
            anchors.leftMargin: Style.space(12)
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.spacing.controlGap

            Button {
              width: Style.space(28)
              height: Style.space(28)
              horizontalPadding: 0
              verticalPadding: 0
              iconText: "󰐍"
              tooltipText: "Continue"
              foreground: root.barForeground
              bordered: true
              selected: root.tab === 0 && !root.settingsMode
              onClicked: { root.tab = 0; root.settingsMode = false }
            }

            Button {
              width: Style.space(28)
              height: Style.space(28)
              horizontalPadding: 0
              verticalPadding: 0
              iconText: "󱉟"
              tooltipText: "Library"
              foreground: root.barForeground
              bordered: true
              selected: root.tab === 1 && !root.settingsMode
              onClicked: { root.tab = 1; root.settingsMode = false }
            }

            Button {
              width: Style.space(28)
              height: Style.space(28)
              horizontalPadding: 0
              verticalPadding: 0
              iconText: "󱀡"
              tooltipText: "Highlights"
              foreground: root.barForeground
              bordered: true
              selected: root.tab === 2 && !root.settingsMode
              onClicked: {
                root.tab = 2
                root.settingsMode = false
                root.maybeLoadRecent()
              }
            }

            Button {
              width: Style.space(28)
              height: Style.space(28)
              horizontalPadding: 0
              verticalPadding: 0
              iconText: "󰒓"
              tooltipText: "Settings"
              foreground: root.barForeground
              bordered: true
              selected: root.settingsMode
              onClicked: root.settingsMode = !root.settingsMode
            }
          }

          Button {
            id: refreshButton
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            width: Style.space(28)
            height: Style.space(28)
            horizontalPadding: 0
            verticalPadding: 0
            iconText: "󰑐"
            iconSpinning: root.refreshing || root.highlightsLoading
            tooltipText: (root.refreshing || root.highlightsLoading) ? "Refreshing…" : "Refresh"
            foreground: root.barForeground
            bordered: true
            selected: root.refreshing || root.highlightsLoading
            onClicked: root.refresh()
          }
        }

        PanelSeparator {
          id: divider
          anchors.top: tabBar.bottom
          anchors.topMargin: shell.gap
          anchors.left: parent.left
          anchors.right: parent.right
          foreground: root.barForeground
        }

        Flickable {
          id: body
          anchors.top: divider.bottom
          anchors.topMargin: shell.gap
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.bottom: parent.bottom
          contentWidth: width
          contentHeight: bodyColumn.implicitHeight
          clip: true
          boundsBehavior: Flickable.StopAtBounds
          flickableDirection: Flickable.VerticalFlick
          interactive: contentHeight > height
          ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

          function scrollBy(step) {
            var maximum = Math.max(0, contentHeight - height)
            contentY = Math.max(0, Math.min(maximum, contentY + step * Style.space(48)))
          }

          Column {
            id: bodyColumn
            width: body.width
            spacing: shell.gap

            Column {
              width: parent.width
              spacing: Style.space(10)
              visible: root.settingsMode

              PanelSectionHeader {
                text: "Amazon session"
                foreground: root.barForeground
                fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
              }

              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                color: root.errorCode !== "" ? Color.urgent : Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
                text: root.errorCode === ""
                  ? "Signed in to read.amazon.com as " + root.configuredRegion + ". Cookies are stored in ~/.config/omakindle/session.json (owner-only)."
                  : (root.errorMessage !== "" ? root.errorMessage : "Session needs attention")
              }

              Row {
                spacing: Style.space(8)

                Button {
                  text: root.kindle && root.kindle.authRunning ? "Waiting…" : "Sign in with Amazon"
                  foreground: root.barForeground
                  fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                  bordered: true
                  selected: !!(root.kindle && root.kindle.authRunning)
                  verticalPadding: Style.spacing.inputPaddingY
                  enabled: !(root.kindle && root.kindle.authRunning)
                  onClicked: {
                    root.saveMessage = ""
                    root.saveFailed = false
                    if (root.kindle) root.kindle.authorize(root.regionInput)
                  }
                }

                Button {
                  visible: !!(root.kindle && root.kindle.authRunning)
                  text: "Cancel"
                  foreground: root.barForeground
                  fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                  bordered: true
                  verticalPadding: Style.spacing.inputPaddingY
                  onClicked: if (root.kindle) root.kindle.cancelAuthorize()
                }
              }

              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                visible: !!(root.kindle && root.kindle.authStatus !== "")
                color: root.kindle && root.kindle.authFailed ? Color.urgent : Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
                text: root.kindle ? String(root.kindle.authStatus) : ""
              }

              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                visible: !!(root.kindle && root.kindle.needsDeviceToken)
                color: Color.urgent
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
                text: "Cookies are saved, but the device token is still missing. Paste the getDeviceToken URL below and press Save to finish setup."
              }

              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                color: Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
                text: "Manual setup: open read.amazon.com in your browser, DevTools → Network, copy the cookie header from any request, and copy the getDeviceToken URL. Paste both below."
              }

              Text {
                text: "Cookies"
                color: Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
              }

              TextField {
                id: cookiesField
                width: parent.width
                placeholderText: "ubid-main=…; at-main=…; x-main=…; session-id=…"
                foreground: root.barForeground
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                text: root.cookiesInput
                onTextChanged: root.cookiesInput = text
              }

              Text {
                text: "Device token or getDeviceToken URL"
                color: Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
              }

              TextField {
                id: tokenField
                width: parent.width
                placeholderText: "https://read.amazon.com/service/web/register/getDeviceToken?serialNumber=…"
                foreground: root.barForeground
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                text: root.tokenInput
                onTextChanged: root.tokenInput = text
              }

              Row {
                spacing: Style.space(8)

                TextField {
                  id: regionField
                  width: Style.space(120)
                  placeholderText: "us"
                  foreground: root.barForeground
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  text: root.regionInput
                  onTextChanged: root.regionInput = text
                }

                Button {
                  text: root.saving ? "Saving…" : "Save session"
                  foreground: root.barForeground
                  fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                  bordered: true
                  selected: root.saveMessage !== "" && !root.saveFailed
                  verticalPadding: Style.spacing.inputPaddingY
                  enabled: !root.saving && root.cookiesInput !== "" && root.tokenInput !== ""
                  onClicked: root.saveCredentials()
                }

                Button {
                  text: "Clear"
                  foreground: root.barForeground
                  fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                  bordered: true
                  verticalPadding: Style.spacing.inputPaddingY
                  enabled: !root.saving && root.errorCode !== "unconfigured"
                  onClicked: root.clearSession()
                }
              }

              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                visible: root.saveMessage !== ""
                color: root.saveFailed ? Color.urgent : Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
                text: root.saveMessage
              }

              PanelSeparator {
                width: parent.width
                foreground: root.barForeground
              }

              PanelSectionHeader {
                text: "Refresh every"
                foreground: root.barForeground
                fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
              }

              Row {
                spacing: Style.space(6)
                Repeater {
                  model: [15, 30, 60, 120]
                  Button {
                    required property int modelData
                    text: modelData + " min"
                    foreground: root.barForeground
                    fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                    bordered: true
                    selected: Number(root.setting("refreshMinutes", 30)) === modelData
                    verticalPadding: Style.spacing.inputPaddingY
                    onClicked: {
                      root.saveSetting("refreshMinutes", modelData)
                      if (root.kindle) root.kindle.setRefreshMinutes(modelData)
                    }
                  }
                }
              }

              PanelSeparator {
                width: parent.width
                foreground: root.barForeground
              }

              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                color: Color.muted
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                font.pixelSize: Style.font.bodySmall
                text: "OmaKindle"
                  + (root.kindle && root.kindle.manifest && root.kindle.manifest.version
                    ? " · v" + String(root.kindle.manifest.version) : "")
                  + " — unofficial, read-only, for personal use"
              }
            }

            Column {
              width: parent.width
              spacing: Style.space(8)
              visible: !root.settingsMode && root.tab === 0

              PanelSectionHeader {
                x: Style.space(14)
                visible: root.reading.length > 0
                text: root.reading.length + " in progress"
                foreground: root.barForeground
                fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
              }

              Item {
                width: parent.width
                height: emptyReading.implicitHeight
                visible: root.reading.length === 0

                Text {
                  id: emptyReading
                  x: Style.space(14)
                  width: parent.width - Style.space(34)
                  wrapMode: Text.WordWrap
                  textFormat: Text.PlainText
                  color: root.errorCode !== "" || root.progressError !== ""
                    ? Color.urgent : Color.muted
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  font.pixelSize: Style.font.bodySmall
                  text: root.errorCode !== ""
                    ? (root.errorMessage !== "" ? root.errorMessage : "OmaKindle needs setup")
                    : (root.progressError !== ""
                      ? root.progressError
                      : (root.refreshing ? "Loading your library…" : "Nothing in progress right now"))
                }
              }

              Item {
                width: parent.width
                height: progressWarning.implicitHeight
                visible: root.reading.length > 0 && root.progressError !== ""

                Text {
                  id: progressWarning
                  x: Style.space(14)
                  width: parent.width - Style.space(34)
                  wrapMode: Text.WordWrap
                  textFormat: Text.PlainText
                  color: Color.urgent
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  font.pixelSize: Style.font.bodySmall
                  text: "Progress may be out of date. " + root.progressError
                }
              }

              Repeater {
                model: root.reading

                Rectangle {
                  required property var modelData
                  width: parent.width
                  height: Style.space(64)
                  radius: Style.cornerRadius
                  color: readingMouse.containsMouse
                    ? Util.alpha(root.barForeground, 0.06) : "transparent"

                  Behavior on color { ColorAnimation { duration: 120 } }

                  MouseArea {
                    id: readingMouse
                    anchors.fill: parent
                    hoverEnabled: true
                    acceptedButtons: Qt.LeftButton
                    onClicked: root.openBook(modelData)
                  }

                  Image {
                    id: readingCover
                    anchors.left: parent.left
                    anchors.leftMargin: Style.space(14)
                    anchors.verticalCenter: parent.verticalCenter
                    width: Style.space(36)
                    height: Style.space(50)
                    source: String(modelData.coverUrl || "")
                    sourceSize.width: 72
                    sourceSize.height: 100
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                    cache: true
                  }

                  Column {
                    id: readingInfo
                    anchors.left: readingCover.right
                    anchors.leftMargin: Style.space(12)
                    anchors.right: readingActions.left
                    anchors.rightMargin: Style.space(12)
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.spacing.sm

                    Text {
                      width: parent.width
                      text: String(modelData.title || "")
                      color: root.barForeground
                      font.family: root.bar ? root.bar.fontFamily : Style.font.family
                      font.pixelSize: Style.font.body
                      elide: Text.ElideRight
                      maximumLineCount: 1
                    }

                    Text {
                      width: parent.width
                      text: (modelData.authors || []).join(", ")
                      color: Color.muted
                      font.family: root.bar ? root.bar.fontFamily : Style.font.family
                      font.pixelSize: Style.font.bodySmall
                      elide: Text.ElideRight
                      maximumLineCount: 1
                    }

                    Item {
                      width: parent.width
                      height: Math.max(readingMeta.implicitHeight, Style.space(4))

                      Rectangle {
                        id: readingTrack
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: Style.space(64)
                        height: Style.space(4)
                        radius: height / 2
                        color: Util.alpha(root.barForeground, 0.12)

                        Rectangle {
                          width: parent.width * Math.max(0, Math.min(100,
                            Number(modelData.percentageRead || 0))) / 100
                          height: parent.height
                          radius: parent.radius
                          color: Color.accent
                        }
                      }

                      Text {
                        id: readingMeta
                        anchors.left: readingTrack.right
                        anchors.leftMargin: Style.spacing.sm
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        textFormat: Text.PlainText
                        text: Api.progressMeta(modelData.percentageRead, modelData.deviceName)
                        color: Color.muted
                        font.family: root.bar ? root.bar.fontFamily : Style.font.family
                        font.pixelSize: Style.font.caption
                        elide: Text.ElideRight
                        maximumLineCount: 1
                      }
                    }
                  }

                  Row {
                    id: readingActions
                    anchors.right: parent.right
                    anchors.rightMargin: Style.space(10)
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.spacing.controlGap

                    PanelActionButton {
                      iconText: "󱀡"
                      tooltipText: "Quotes"
                      foreground: root.barForeground
                      bordered: true
                      size: Style.space(28)
                      onClicked: root.loadHighlights(modelData)
                    }

                    PanelActionButton {
                      iconText: "󰗚"
                      tooltipText: "Read"
                      foreground: root.barForeground
                      bordered: true
                      size: Style.space(28)
                      onClicked: root.openBook(modelData)
                    }
                  }
                }
              }
            }

            Column {
              width: parent.width
              spacing: Style.space(8)
              visible: !root.settingsMode && root.tab === 1

              TextField {
                id: searchField
                width: parent.width
                placeholderText: "Search title or author"
                foreground: root.barForeground
                font.family: root.bar ? root.bar.fontFamily : Style.font.family
                onTextChanged: root.search = text
              }

              PanelSectionHeader {
                x: Style.space(14)
                text: root.filteredBooks.length + " of " + root.books.length + " books"
                foreground: root.barForeground
                fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
              }

              Repeater {
                model: root.filteredBooks

                Rectangle {
                  required property var modelData
                  width: parent.width
                  height: Style.space(56)
                  radius: Style.cornerRadius
                  color: libraryMouse.containsMouse
                    ? Util.alpha(root.barForeground, 0.06) : "transparent"

                  Behavior on color { ColorAnimation { duration: 120 } }

                  MouseArea {
                    id: libraryMouse
                    anchors.fill: parent
                    hoverEnabled: true
                    acceptedButtons: Qt.LeftButton
                    onClicked: root.openBook(modelData)
                  }

                  Image {
                    id: libraryCover
                    anchors.left: parent.left
                    anchors.leftMargin: Style.space(14)
                    anchors.verticalCenter: parent.verticalCenter
                    width: Style.space(32)
                    height: Style.space(44)
                    source: String(modelData.coverUrl || "")
                    sourceSize.width: 64
                    sourceSize.height: 88
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                    cache: true
                  }

                  Column {
                    id: libraryInfo
                    anchors.left: libraryCover.right
                    anchors.leftMargin: Style.space(12)
                    anchors.right: libraryActions.left
                    anchors.rightMargin: Style.space(12)
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.spacing.sm

                    Text {
                      width: parent.width
                      text: String(modelData.title || "")
                      color: root.barForeground
                      font.family: root.bar ? root.bar.fontFamily : Style.font.family
                      font.pixelSize: Style.font.body
                      elide: Text.ElideRight
                      maximumLineCount: 1
                    }

                    Text {
                      width: parent.width
                      text: (modelData.authors || []).join(", ")
                      color: Color.muted
                      font.family: root.bar ? root.bar.fontFamily : Style.font.family
                      font.pixelSize: Style.font.bodySmall
                      elide: Text.ElideRight
                      maximumLineCount: 1
                    }
                  }

                  Row {
                    id: libraryActions
                    anchors.right: parent.right
                    anchors.rightMargin: Style.space(10)
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.spacing.controlGap

                    PanelActionButton {
                      iconText: "󱀡"
                      tooltipText: "Quotes"
                      foreground: root.barForeground
                      bordered: true
                      size: Style.space(28)
                      onClicked: root.loadHighlights(modelData)
                    }

                    PanelActionButton {
                      iconText: "󰗚"
                      tooltipText: "Read"
                      foreground: root.barForeground
                      bordered: true
                      size: Style.space(28)
                      onClicked: root.openBook(modelData)
                    }
                  }
                }
              }
            }

            Column {
              width: parent.width
              spacing: Style.space(8)
              visible: !root.settingsMode && root.tab === 2

              Item {
                width: parent.width
                height: Math.max(highlightsHeading.implicitHeight,
                  highlightsHeadingActions.implicitHeight)

                Text {
                  id: highlightsHeading
                  anchors.left: parent.left
                  anchors.right: highlightsHeadingActions.left
                  anchors.rightMargin: Style.space(8)
                  anchors.verticalCenter: parent.verticalCenter
                  text: root.recentMode
                    ? "Recent highlights"
                    : (root.highlightsTitle !== "" ? root.highlightsTitle : "Highlights")
                  color: root.barForeground
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  font.pixelSize: Style.font.body
                  font.bold: true
                  elide: Text.ElideRight
                  maximumLineCount: 1
                }

                Row {
                  id: highlightsHeadingActions
                  anchors.right: parent.right
                  anchors.rightMargin: Style.space(10)
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(6)

                  Button {
                    visible: !root.recentMode
                    text: "All books"
                    foreground: root.barForeground
                    fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                    bordered: true
                    verticalPadding: Style.spacing.inputPaddingY
                    onClicked: root.loadRecentHighlights()
                  }

                }
              }

              Item {
                width: parent.width
                height: recentStatus.implicitHeight
                visible: root.recentMode && root.recentHighlights.length === 0

                Text {
                  id: recentStatus
                  width: parent.width - Style.space(20)
                  wrapMode: Text.WordWrap
                  textFormat: Text.PlainText
                  color: Color.muted
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  font.pixelSize: Style.font.bodySmall
                  text: root.recentLoading
                    ? (root.recentScanned > 0
                      ? "Collecting highlights… " + root.recentScanned + " books checked"
                      : "Collecting highlights from your recent books…")
                    : "No highlights found in your recent books"
                }
              }

              Item {
                width: parent.width
                height: selectedStatus.implicitHeight
                visible: !root.recentMode && (root.highlightsLoading
                  || root.highlightsUpdating
                  || root.highlightsError !== ""
                  || (root.highlights && root.highlights.count === 0))

                Text {
                  id: selectedStatus
                  width: parent.width - Style.space(20)
                  wrapMode: Text.WordWrap
                  textFormat: Text.PlainText
                  color: root.highlightsError !== "" ? Color.urgent : Color.muted
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  font.pixelSize: Style.font.bodySmall
                  text: root.highlightsLoading
                    ? "Loading highlights…"
                    : (root.highlightsError !== ""
                      ? root.highlightsError
                      : (root.highlightsUpdating
                        ? "Refreshing highlights…"
                        : "No highlights for this book yet"))
                }
              }

              Item {
                width: parent.width
                height: limitedNotice.implicitHeight
                visible: !root.recentMode && root.highlights && root.highlights.limited === true
                  && root.highlights.count > 0

                Text {
                  id: limitedNotice
                  width: parent.width - Style.space(20)
                  wrapMode: Text.WordWrap
                  textFormat: Text.PlainText
                  color: Color.muted
                  font.family: root.bar ? root.bar.fontFamily : Style.font.family
                  font.pixelSize: Style.font.bodySmall
                  text: "Some highlights are previews; use Copy to fetch the full passage from Amazon."
                }
              }

              Repeater {
                model: root.recentMode ? root.recentGroups : []

                Column {
                  required property var modelData
                  width: parent.width
                  spacing: Style.spacing.sm

                  PanelSectionHeader {
                    width: parent.width
                    visible: String(modelData.title || "") !== ""
                    text: String(modelData.title || "")
                    foreground: root.barForeground
                    fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
                    elide: Text.ElideRight
                    maximumLineCount: 1
                  }

                  Repeater {
                    model: modelData.items

                    QuoteCard {
                      required property var modelData
                      entry: modelData
                    }
                  }
                }
              }

              Repeater {
                model: root.recentMode ? [] : root.displayedHighlights

                QuoteCard {
                  required property var modelData
                  entry: modelData
                }
              }
            }
          }
        }
      }
    }
  }
}
