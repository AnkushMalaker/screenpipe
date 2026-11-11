// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

// One capture transaction. No global compositor preferences are modified.
// CaptureWindow supplies isolated surfaces; signals make even an ABA policy
// change invalidate the transaction before the pixels can reach storage.
var changed = false;
function invalidate() { changed = true; }
function watch(object, names) {
    names.forEach(function (name) {
        if (!object[name] || !object[name].connect) throw new Error("missing privacy signal " + name);
        object[name].connect(invalidate);
    });
}
function rect(r) { return {x:r.x, y:r.y, width:r.width, height:r.height}; }
function snapshot() {
    var windows = workspace.stackingOrder;
    return {
        outputs: workspace.screens.map(function (s) { return {name:String(s.name), rect:rect(s.geometry)}; }),
        windows: windows.map(function (w) {
            var desktops = w.desktops;
            var onDesktop = w.onAllDesktops || desktops.indexOf(workspace.currentDesktop) >= 0;
            var activities = w.activities;
            return {id:String(w.internalId), app:w.resourceClass ? String(w.resourceClass) : "", title:w.caption ? String(w.caption) : "", pid:w.pid,
                rect:rect(w.frameGeometry), opacity:w.opacity,
                visible:!w.deleted && !w.minimized && !w.hidden && onDesktop &&
                    (activities.length === 0 || activities.indexOf(workspace.currentActivity) >= 0),
                protected:w.excludeFromCapture === true};
        })
    };
}
try {
    watch(workspace, ["windowAdded", "windowRemoved", "currentDesktopChanged", "currentActivityChanged", "screensChanged"]);
    workspace.stackingOrder.forEach(function (w) {
        watch(w, ["captionChanged", "windowClassChanged", "frameGeometryChanged", "opacityChanged",
            "stackingOrderChanged", "desktopsChanged", "activitiesChanged", "minimizedChanged",
            "hiddenChanged", "readyForPaintingChanged", "closed", "surfaceChanged", "excludeFromCaptureChanged"]);
    });
    var before = JSON.stringify(snapshot());
    callDBus(DESTINATION, "/com/screenpipe/PrivacyCapture", "com.screenpipe.PrivacyCapture", "Begin", before,
        function () {
            try {
                callDBus(DESTINATION, "/com/screenpipe/PrivacyCapture", "com.screenpipe.PrivacyCapture", "Finish", JSON.stringify(snapshot()), changed);
            } catch (e) {
                callDBus(DESTINATION, "/com/screenpipe/PrivacyCapture", "com.screenpipe.PrivacyCapture", "Abort", String(e.message));
            }
        });
} catch (e) {
    callDBus(DESTINATION, "/com/screenpipe/PrivacyCapture", "com.screenpipe.PrivacyCapture", "Abort", String(e.message));
}
