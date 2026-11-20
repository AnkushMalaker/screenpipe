// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
// One synchronous compositor snapshot; no asynchronous cached focus is admitted.
function rect(r) { return {x:r.x, y:r.y, width:r.width, height:r.height}; }
var result = {window:null, outputs:[]};
try {
    var w = workspace.activeWindow;
    if (w && !w.deleted && !w.minimized && !w.hidden) {
        result.window = rect(w.frameGeometry);
        result.outputs = workspace.screens.map(function(s) {
            return {name:String(s.name), rect:rect(s.geometry)};
        });
    }
} catch (e) { result = {window:null, outputs:[]}; }
callDBus(destination, "/com/screenpipe/FocusSnapshot", "com.screenpipe.FocusSnapshot", "Snapshot", JSON.stringify(result));
