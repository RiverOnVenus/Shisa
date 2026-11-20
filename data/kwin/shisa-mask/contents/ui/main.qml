import QtQuick
import QtQuick.Window
import org.kde.plasma.core as PlasmaCore
import org.kde.plasma.workspace.dbus as DBus

QtObject {
    id: root

    property var masks: ({})
    property bool polling: false

    function call(member, args, resolve, reject) {
        DBus.SessionBus.asyncCall({
            service: "io.github.RiverOnVenus.Shisa.Window",
            path: "/io/github/RiverOnVenus/Shisa/Window",
            iface: "io.github.RiverOnVenus.Shisa.Window",
            member: member,
            arguments: args
        }, resolve, reject)
    }

    function update(rows) {
        const next = {};
        for (const row of rows) {
            const [id, x, y, width, height] = row;
            const mask = masks[id] || cover.createObject(root);
            mask.setGeometry(x, y, width, height);
            next[id] = mask;
        }
        for (const id of Object.keys(masks)) {
            if (!next[id])
                masks[id].destroy();
        }
        masks = next;
    }

    property Timer poll: Timer {
        interval: 250
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (root.polling)
                return;
            root.polling = true;
            root.call("PrivacyMasks", [], reply => {
                root.polling = false;
                root.update(reply.value ? JSON.parse(reply.value) : []);
            }, () => {
                root.polling = false;
                root.update([]);
            });
        }
    }

    property Component cover: Component {
        PlasmaCore.Window {
            id: mask
            title: "Shisa Privacy Mask"
            flags: Qt.X11BypassWindowManagerHint | Qt.FramelessWindowHint
                | Qt.WindowDoesNotAcceptFocus | Qt.WindowTransparentForInput
            backgroundHints: PlasmaCore.Types.NoBackground
            property bool positioned: false
            visible: positioned && width > 0 && height > 0

            function setGeometry(left, top, wide, tall) {
                x = left;
                y = top;
                width = wide;
                height = tall;
                positioned = true;
            }

            mainItem: Rectangle {
                color: "#20242c"
                Text {
                    anchors.centerIn: parent
                    text: "Shisa"
                    color: "#8b93a3"
                    font.pixelSize: 14
                    font.letterSpacing: 1
                }
            }
        }
    }
}
