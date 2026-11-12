# VST3 editors

The host opens a plugin's native editor from its project preset. The preset
inspector also exposes parameters when a plugin has no editor or cannot
attach one. See [Plugins](manual/plugins.md) to load an instrument and save
its state.

## Platform backends

- **macOS:** The editor attaches an NSView to an AppKit window on the main
  thread. OpenCompose uses the in-process editor by default.
- **Linux:** The editor helper attaches through X11. A Wayland session needs
  XWayland and a plugin that supports the X11 embedding interface.
- **Windows:** The editor helper attaches the view to an HWND. It handles
  native resize and close events.

The helper applies component state before it creates the view. It reports
success once the view attaches, and it exits when the window closes. A helper
failure is reported to the application.

## Window lifecycle

The host installs an `IPlugFrame` and asks for the initial size before attach.
A plugin that reports its size only after attach opens at a temporary size.
The host then adopts the plugin's size and resize policy. A resize requested
during attach is deferred until the attach callback returns. On close, the
host detaches the view and clears the frame before disposing of the window.

The host supplies VST3 services such as `IMessage` through the host
application interface. Processor, state, bus-layout, initialization, connection
and termination calls use native C++ exception barriers, as do references owned
by the guarded core interface holder. A caught native failure
quarantines the instance: later guarded calls stop, and retained plugin references
and callback backing are kept alive. Ordinary acquisition refusals clean up the
phases that completed; a cleanup failure is reported alongside the original error.

Editor controller handles share the instance's failure authority. Their final
worker-thread drop queues native reference retirement for the AppKit main thread.
Native view owners remain in a main-thread registry until explicit close; shared
callback handles contain no native view owner. Registry keys keep a strong live
identity anchor, so pointer reuse cannot retire another editor's view. Unretired
registry or queued owners are deliberately retained at shutdown instead of calling
plugins during thread-local destruction or from a worker. Native view
methods and view references, factory/module exports, controller handler setup
and controller parameter metadata still have
unresolved exception boundaries. Editor methods must run on the platform's UI
thread. These in-process barriers do not contain memory corruption, access
violations, aborts or hangs; helper-process isolation provides a separate boundary.

## Parameter edits and audition

Controller edits from an in-process editor update the preset's parameter bus.
The renderer reads coalesced edits at the start of a block. Saving a preset
flushes the latest edits into the processor before capturing component state.
The helper forwards edits on Unix. The Windows helper has no edit relay.

On macOS the in-process editor's instance can be monitored when the project
is stopped. This signal is dry and bypasses the project mixer and effects.
While the project plays, the monitor is muted and the arrangement's own
instance supplies audio. Audio from editor helper processes is not monitored
by OpenCompose.

## Verification

The `oc-host` editor tests cover attach order, late sizing, resize callbacks,
and close order. The parameter tests cover forwarding edits into audio
processing and capturing them in preset state. Native window behavior also
depends on the plugin and platform.
