import QtQuick
import QtQuick.Effects
import qs.Commons
import qs.Ui

import "Api.js" as Api

BarWidget {
  id: root

  moduleName: "minholi.kindle"

  readonly property var kindle: bar && bar.shell
    ? bar.shell.serviceFor("minholi.kindle") : null
  readonly property var panelItem: panelLoader.item
  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property string barStyle: String(setting("barStyle", "Cover only"))
  readonly property bool showProgress: String(setting("showProgress", "On")) !== "Off"
  readonly property var current: kindle && kindle.current ? kindle.current : null
  readonly property string label: Api.barLabel(current, showProgress)
  readonly property bool hasBook: !!current && label !== ""
  readonly property bool coverEnabled: barStyle === "Cover only" || barStyle === "Cover and title"
  readonly property bool titleEnabled: barStyle === "Cover and title" || barStyle === "Title only"
  readonly property bool showCover: coverEnabled && hasBook
  readonly property bool showLabel: titleEnabled && hasBook && !root.vertical && label !== ""
  readonly property bool showIcon: !showCover && !showLabel
  readonly property string coverSource: showCover && current && current.coverUrl
    ? String(current.coverUrl) : ""
  readonly property bool busy: kindle ? kindle.refreshing : false
  readonly property string errorCode: kindle ? kindle.errorCode : ""
  readonly property bool needsSetup: !!errorCode && errorCode !== ""

  function injectPanel() {
    var target = panelLoader.item
    if (!target) return
    if ("bar" in target) target.bar = root.bar
    if ("settings" in target) target.settings = root.settings
    if ("anchorItem" in target) target.anchorItem = button
    if ("hostWidget" in target) target.hostWidget = root
  }

  function refresh() {
    if (panelItem) panelItem.refresh()
  }

  function toggle() {
    if (panelItem) panelItem.toggle()
  }

  readonly property bool opened: panelItem ? panelItem.opened === true : false

  function open() {
    if (panelItem && panelItem.openFromHotkey) panelItem.openFromHotkey()
  }

  function close() {
    if (panelItem && panelItem.close) panelItem.close()
  }

  readonly property bool popoutSwitchClosing: panelItem
    ? panelItem.popoutSwitchClosing === true : false

  function closeForPopoutSwitch() {
    if (panelItem) panelItem.closeForPopoutSwitch()
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  onBarChanged: injectPanel()
  onSettingsChanged: injectPanel()

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: {
      root.injectPanel()
      Qt.callLater(root.injectPanel)
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: ""
    labelVisible: false
    hasVisualContent: true
    dimmed: !root.hasBook
    fixedWidth: root.vertical ? -1 : content.implicitWidth + button.scaledHorizontalMargin * 2
    tooltipText: root.hasBook
      ? root.label
      : (root.needsSetup ? "Kindle needs setup" : "Kindle")

    onPressed: function(buttonType) {
      if (!root.bar) return
      if (buttonType === Qt.MiddleButton) root.refresh()
      else root.toggle()
    }

    Row {
      id: content
      anchors.centerIn: parent
      spacing: Style.space(6)

      Item {
        visible: root.showCover || root.showIcon
        width: root.showCover ? Style.space(16) : Style.space(14)
        height: Style.space(20)
        anchors.verticalCenter: parent.verticalCenter

        Image {
          id: bookImage
          anchors.fill: parent
          source: Qt.resolvedUrl("assets/book.svg")
          sourceSize.width: 32
          sourceSize.height: 32
          fillMode: Image.PreserveAspectFit
          visible: false
        }

        MultiEffect {
          anchors.fill: bookImage
          source: bookImage
          visible: root.showIcon
          colorizationColor: root.foreground
          colorization: 1.0
        }

        Image {
          anchors.fill: parent
          source: root.coverSource
          sourceSize.width: 32
          sourceSize.height: 40
          fillMode: Image.PreserveAspectCrop
          visible: root.showCover
          asynchronous: true
          cache: true
        }
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        visible: root.showLabel
        text: root.label
        color: root.foreground
        font.family: root.bar ? root.bar.fontFamily : Style.font.family
        font.pixelSize: Style.font.bodySmall
        elide: Text.ElideRight
        maximumLineCount: 1
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        visible: root.busy && root.showLabel
        text: "…"
        color: Color.muted
        font.family: root.bar ? root.bar.fontFamily : Style.font.family
        font.pixelSize: Style.font.bodySmall
      }
    }
  }
}
