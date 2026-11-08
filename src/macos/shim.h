// Shared between shim.m (Metal, menu bar, title bar, hotkey) and viewer.m
// (the built-in viewers).

#import <AppKit/AppKit.h>

// Holds a window's viewers over its frames; implemented in viewer.m.
@interface TeminoViewerHost : NSView
@end

// Rounds the frames' layer to the window's corners; in shim.m.
void temino_round_corners(NSWindow* window);
