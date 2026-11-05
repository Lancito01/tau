# Components

Orbit wheel (replaces the original's sliced disc: separate round nodes on a thin orbit, never wedges)
  parts     dark disc, thin orbit ring, 1-9 round nodes, centre hub, tether from hub to the active node, comet arc on the orbit
  states    probing (invisible), opening (nodes fly out along spokes), idle (no selection in the hub), active (node grows, fills accent), unavailable (dimmed, reason in hub), empty (hub text only)
  selection by angle anywhere outside the hub; the hub is the cancel zone
  keyboard  arrows step, Enter applies, Esc cancels, bound keys switch wheels

Progress HUD
  parts     title, "n of m saved", bar, close button
  states    running, done (click reveals the file), error (stays longer), cancelled

Option form    native Win32 controls; label + number, choice, text, check; OK and Cancel
Canvas editor  image with drag rectangles: crop, redact, annotate
Timeline       thumbnail or waveform strip with handles: trim, split, snapshot, bleep
Shelf          always-on-top file list, drag out, pin to edge
