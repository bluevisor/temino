// macOS: the built-in viewers (text editor, web view, image viewer) that
// sit over a pane, behind a small C interface for src/platform/desktop.rs.

#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#import <CoreText/CoreText.h>
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>
#import <WebKit/WebKit.h>
#import <objc/message.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#import "shim.h"

// MARK: Viewers

// Built-in viewers: a text editor, a web view and an image viewer, each a
// native view with its own header bar, placed by the core thread over a
// pane's spot. A new one slides in from the window's right edge on Core
// Animation, so the header and the content move as one.

typedef void (*TeminoViewerCallback)(uint64_t id, int32_t event, const char* text);

// As ViewerStyle in src/platform/mod.rs.
typedef struct {
	float background[3];
	float text[3];
	float accent[3];
	float selection[3];
	float header[3];
	float divider[3];
	float dim[3];
	float faint[3];
	float font_size;
	bool dark;
} TeminoViewerStyle;

// Kinds and events, as in src/platform/mod.rs and desktop.rs.
enum { KIND_TEXT = 0, KIND_WEB = 1, KIND_IMAGE = 2 };
enum { EVENT_FOCUSED = 0, EVENT_TITLE = 1, EVENT_URL = 2, EVENT_DIRTY = 3, EVENT_CLEAN = 4, EVENT_FAILED = 5, EVENT_CLOSE = 6 };

static const CGFloat kViewerHeader = 30;
static const CGFloat kViewerButton = 24;
static const NSTimeInterval kViewerSlide = 0.3;

static TeminoViewerCallback sViewerCallback;
// Every window's viewers, by id: the window's number in the top 32 bits,
// the pane's below. And each window's host for them, by window number.
static NSMutableDictionary<NSNumber*, id>* sViewers;
static NSMutableDictionary<NSNumber*, NSView*>* sViewerHosts;

static NSView*
host_for(uint64_t id)
{
	return sViewerHosts[@((uint32_t)(id >> 32))];
}

static void
viewer_send(uint64_t id, int32_t event, NSString* text)
{
	if (sViewerCallback != NULL)
		sViewerCallback(id, event, text != nil ? text.UTF8String : "");
}

static NSColor*
style_color(const float rgb[3])
{
	return [NSColor colorWithSRGBRed:rgb[0] green:rgb[1] blue:rgb[2] alpha:1];
}

// `path` with the home folder as ~.
static NSString*
abbreviated(NSString* path)
{
	return path.stringByAbbreviatingWithTildeInPath;
}

// Covers winit's view in top-left coordinates, letting every event through
// except over a viewer.

@implementation TeminoViewerHost
- (BOOL)isFlipped
{
	return YES;
}

- (NSView*)hitTest:(NSPoint)point
{
	NSView* hit = [super hitTest:point];
	return hit == self ? nil : hit;
}
@end

@interface TeminoFlippedView : NSView
@end

@implementation TeminoFlippedView
- (BOOL)isFlipped
{
	return YES;
}
@end


@interface TeminoViewer : NSView <NSTextViewDelegate, WKNavigationDelegate, WKUIDelegate>
@property(nonatomic) uint64_t viewerId;
@property(nonatomic) int kind;
// The file's path, or the page's address.
@property(nonatomic, copy) NSString* path;
@property(nonatomic, strong) NSView* header;
@property(nonatomic, strong) NSView* body;
@property(nonatomic, strong) NSTextField* titleLabel;
@property(nonatomic, strong) NSTextField* placeLabel;
@property(nonatomic, strong) NSView* dirtyDot;
// Right to left.
@property(nonatomic, strong) NSArray<NSButton*>* buttons;
// What takes the keyboard.
@property(nonatomic, strong) NSView* focusView;
@property(nonatomic, strong) NSTextView* textView;
@property(nonatomic, strong) WKWebView* webView;
// The file is RTF (or RTFD), kept rich.
@property(nonatomic) BOOL rich;
@property(nonatomic) BOOL dirty;
// Asked for the keyboard while hidden: takes it once it shows.
@property(nonatomic) BOOL pendingFocus;
// New: slides in when it first shows.
@property(nonatomic) BOOL slideIn;
@end

@implementation TeminoViewer
- (BOOL)isFlipped
{
	return YES;
}

- (void)dealloc
{
	if (_webView != nil) {
		[_webView removeObserver:self forKeyPath:@"title"];
		[_webView removeObserver:self forKeyPath:@"URL"];
	}
}

- (NSButton*)buttonWithSymbol:(NSString*)symbol label:(NSString*)label action:(SEL)action style:(const TeminoViewerStyle*)style
{
	NSImage* image = [NSImage imageWithSystemSymbolName:symbol accessibilityDescription:label];
	NSImageSymbolConfiguration* config = [NSImageSymbolConfiguration configurationWithPointSize:12 weight:NSFontWeightMedium];
	NSButton* button = [NSButton buttonWithImage:[image imageWithSymbolConfiguration:config] target:self action:action];
	button.bordered = NO;
	button.contentTintColor = style_color(style->dim);
	button.toolTip = label;
	button.frame = NSMakeRect(0, 0, kViewerButton, kViewerButton);
	[self.header addSubview:button];
	return button;
}

- (void)buildWithStyle:(const TeminoViewerStyle*)style
{
	NSRect bounds = self.bounds;
	TeminoFlippedView* header = [[TeminoFlippedView alloc] initWithFrame:NSMakeRect(0, 0, NSWidth(bounds), kViewerHeader)];
	header.wantsLayer = YES;
	header.layer.backgroundColor = style_color(style->header).CGColor;
	header.autoresizingMask = NSViewWidthSizable;
	NSView* line = [[NSView alloc] initWithFrame:NSMakeRect(0, kViewerHeader - 1, NSWidth(bounds), 1)];
	line.wantsLayer = YES;
	line.layer.backgroundColor = style_color(style->divider).CGColor;
	line.autoresizingMask = NSViewWidthSizable;
	[header addSubview:line];
	[self addSubview:header];
	self.header = header;

	NSTextField* title = [NSTextField labelWithString:@""];
	title.font = [NSFont boldSystemFontOfSize:12];
	title.textColor = style_color(style->text);
	title.lineBreakMode = NSLineBreakByTruncatingTail;
	[header addSubview:title];
	self.titleLabel = title;
	NSTextField* place = [NSTextField labelWithString:@""];
	place.font = [NSFont systemFontOfSize:11];
	place.textColor = style_color(style->faint);
	place.lineBreakMode = NSLineBreakByTruncatingMiddle;
	[header addSubview:place];
	self.placeLabel = place;
	NSView* dot = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 6, 6)];
	dot.wantsLayer = YES;
	dot.layer.backgroundColor = style_color(style->accent).CGColor;
	dot.layer.cornerRadius = 3;
	dot.hidden = YES;
	dot.toolTip = @"Unsaved changes";
	[header addSubview:dot];
	self.dirtyDot = dot;

	NSMutableArray* buttons = [NSMutableArray array];
	[buttons addObject:[self buttonWithSymbol:@"xmark" label:@"Close" action:@selector(closeClicked:) style:style]];
	[buttons addObject:[self buttonWithSymbol:@"arrow.up.forward.app" label:@"Open in Default App"
		action:@selector(externalClicked:) style:style]];
	if (self.kind == KIND_WEB) {
		[buttons addObject:[self buttonWithSymbol:@"arrow.clockwise" label:@"Reload" action:@selector(reloadClicked:) style:style]];
		[buttons addObject:[self buttonWithSymbol:@"chevron.forward" label:@"Forward" action:@selector(forwardClicked:) style:style]];
		[buttons addObject:[self buttonWithSymbol:@"chevron.backward" label:@"Back" action:@selector(backClicked:) style:style]];
	}
	self.buttons = buttons;

	TeminoFlippedView* body = [[TeminoFlippedView alloc]
		initWithFrame:NSMakeRect(0, kViewerHeader, NSWidth(bounds), MAX(0, NSHeight(bounds) - kViewerHeader))];
	body.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
	[self addSubview:body];
	self.body = body;
	[self showTitle:self.path.lastPathComponent place:nil];
}

// The header's title, where it is (the file's folder, the page's
// address) and the unsaved dot, laid out between the edge and the buttons.
- (void)showTitle:(NSString*)title place:(NSString*)place
{
	if (title != nil)
		self.titleLabel.stringValue = title;
	if (place != nil)
		self.placeLabel.stringValue = place;
	[self layoutHeader];
}

- (void)layoutHeader
{
	CGFloat width = NSWidth(self.header.bounds);
	CGFloat right = width - 6;
	for (NSButton* button in self.buttons) {
		right -= kViewerButton;
		button.frame = NSMakeRect(right, (kViewerHeader - kViewerButton) / 2, kViewerButton, kViewerButton);
		button.hidden = right < 60;
		right -= 2;
	}
	CGFloat x = 12;
	CGFloat room = MAX(0, right - x - 8);
	[self.titleLabel sizeToFit];
	CGFloat titleWidth = MIN(NSWidth(self.titleLabel.frame), room);
	CGFloat height = NSHeight(self.titleLabel.frame);
	self.titleLabel.frame = NSMakeRect(x, round((kViewerHeader - height) / 2), titleWidth, height);
	x += titleWidth + 6;
	if (self.dirty) {
		self.dirtyDot.frame = NSMakeRect(x, kViewerHeader / 2 - 3, 6, 6);
		x += 12;
	}
	self.dirtyDot.hidden = !self.dirty;
	[self.placeLabel sizeToFit];
	height = NSHeight(self.placeLabel.frame);
	CGFloat placeWidth = MIN(NSWidth(self.placeLabel.frame), MAX(0, right - x - 8));
	self.placeLabel.frame = NSMakeRect(x, round((kViewerHeader - height) / 2) + 1, placeWidth, height);
	self.placeLabel.hidden = placeWidth < 40;
}

- (void)setFrameSize:(NSSize)size
{
	BOOL changed = size.width != NSWidth(self.frame);
	[super setFrameSize:size];
	if (changed)
		[self layoutHeader];
}

- (void)closeClicked:(id)sender
{
	viewer_send(self.viewerId, EVENT_CLOSE, nil);
}

- (void)externalClicked:(id)sender
{
	if (self.dirty)
		[self save];
	NSURL* url = [self.path hasPrefix:@"/"] ? [NSURL fileURLWithPath:self.path] : [NSURL URLWithString:self.path];
	if (url != nil)
		[NSWorkspace.sharedWorkspace openURL:url];
}

- (void)backClicked:(id)sender
{
	[self.webView goBack];
}

- (void)forwardClicked:(id)sender
{
	[self.webView goForward];
}

- (void)reloadClicked:(id)sender
{
	[self.webView reload];
}

- (void)openText:(const TeminoViewerStyle*)style
{
	NSString* path = self.path;
	NSScrollView* scroll = [NSTextView scrollableTextView];
	scroll.frame = self.body.bounds;
	scroll.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
	scroll.drawsBackground = YES;
	scroll.backgroundColor = style_color(style->background);
	NSTextView* text = scroll.documentView;
	text.delegate = self;
	text.allowsUndo = YES;
	text.usesFindBar = YES;
	text.incrementalSearchingEnabled = YES;
	text.textContainerInset = NSMakeSize(10, 10);
	text.backgroundColor = style_color(style->background);
	text.insertionPointColor = style_color(style->accent);
	text.selectedTextAttributes = @{NSBackgroundColorAttributeName: style_color(style->selection)};

	NSString* extension = path.pathExtension.lowercaseString;
	self.rich = [extension isEqual:@"rtf"] || [extension isEqual:@"rtfd"];
	NSError* error = nil;
	if (self.rich) {
		NSAttributedString* content = [[NSAttributedString alloc] initWithURL:[NSURL fileURLWithPath:path]
			options:@{} documentAttributes:nil error:&error];
		text.richText = YES;
		text.importsGraphics = YES;
		// Dark text in the document stays readable on a dark theme.
		text.usesAdaptiveColorMappingForDarkAppearance = YES;
		if (content != nil)
			[text.textStorage setAttributedString:content];
	} else {
		NSString* content = [NSString stringWithContentsOfFile:path encoding:NSUTF8StringEncoding error:&error];
		text.richText = NO;
		text.automaticQuoteSubstitutionEnabled = NO;
		text.automaticDashSubstitutionEnabled = NO;
		text.automaticTextReplacementEnabled = NO;
		text.automaticSpellingCorrectionEnabled = NO;
		text.continuousSpellCheckingEnabled = NO;
		text.smartInsertDeleteEnabled = NO;
		NSFont* font = [NSFont monospacedSystemFontOfSize:style->font_size weight:NSFontWeightRegular];
		text.font = font;
		text.textColor = style_color(style->text);
		text.typingAttributes = @{NSFontAttributeName: font, NSForegroundColorAttributeName: style_color(style->text)};
		if (content != nil)
			text.string = content;
	}
	if (error != nil)
		viewer_send(self.viewerId, EVENT_FAILED, error.localizedDescription);
	[text.undoManager removeAllActions];
	[self.body addSubview:scroll];
	self.textView = text;
	self.focusView = text;
	[self showTitle:nil place:abbreviated(path.stringByDeletingLastPathComponent)];
}

- (void)openWeb
{
	NSString* target = self.path;
	WKWebViewConfiguration* config = [[WKWebViewConfiguration alloc] init];
	WKWebView* web = [[WKWebView alloc] initWithFrame:self.body.bounds configuration:config];
	web.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
	web.navigationDelegate = self;
	web.UIDelegate = self;
	web.allowsBackForwardNavigationGestures = YES;
	web.allowsMagnification = YES;
	[web addObserver:self forKeyPath:@"title" options:0 context:NULL];
	[web addObserver:self forKeyPath:@"URL" options:0 context:NULL];
	[self.body addSubview:web];
	self.webView = web;
	self.focusView = web;
	if ([target hasPrefix:@"/"]) {
		NSURL* url = [NSURL fileURLWithPath:target];
		[web loadFileURL:url allowingReadAccessToURL:url.URLByDeletingLastPathComponent];
	} else {
		NSURL* url = [NSURL URLWithString:target];
		if (url != nil)
			[web loadRequest:[NSURLRequest requestWithURL:url]];
		[self showTitle:url.host place:target];
	}
}

- (void)openImage:(const TeminoViewerStyle*)style
{
	NSString* path = self.path;
	NSImage* image = [[NSImage alloc] initWithContentsOfFile:path];
	if (image == nil)
		viewer_send(self.viewerId, EVENT_FAILED, [NSString stringWithFormat:@"Can't read %@ as an image", path.lastPathComponent]);
	// Fit to the pane (never enlarged); pinch to zoom in.
	NSScrollView* scroll = [[NSScrollView alloc] initWithFrame:self.body.bounds];
	scroll.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
	scroll.allowsMagnification = YES;
	scroll.minMagnification = 1;
	scroll.maxMagnification = 16;
	scroll.hasVerticalScroller = YES;
	scroll.hasHorizontalScroller = YES;
	scroll.autohidesScrollers = YES;
	scroll.backgroundColor = style_color(style->background);
	NSImageView* view = [[NSImageView alloc] initWithFrame:scroll.bounds];
	view.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
	view.image = image;
	view.imageScaling = NSImageScaleProportionallyDown;
	view.animates = YES;
	scroll.documentView = view;
	[self.body addSubview:scroll];
	self.focusView = scroll;
	NSString* size = image != nil ? [NSString stringWithFormat:@"%.0f × %.0f · ", image.size.width, image.size.height] : @"";
	[self showTitle:nil place:[size stringByAppendingString:abbreviated(path.stringByDeletingLastPathComponent)]];
}

- (void)setDirtyState:(BOOL)dirty
{
	if (self.dirty == dirty)
		return;
	self.dirty = dirty;
	[self layoutHeader];
	viewer_send(self.viewerId, dirty ? EVENT_DIRTY : EVENT_CLEAN, nil);
}

- (void)save
{
	if (self.textView == nil)
		return;
	NSURL* url = [NSURL fileURLWithPath:self.path];
	NSError* error = nil;
	BOOL ok;
	if (self.rich) {
		NSTextStorage* storage = self.textView.textStorage;
		NSString* type = [self.path.pathExtension.lowercaseString isEqual:@"rtfd"] ? NSRTFDTextDocumentType : NSRTFTextDocumentType;
		NSFileWrapper* wrapper = [storage fileWrapperFromRange:NSMakeRange(0, storage.length)
			documentAttributes:@{NSDocumentTypeDocumentAttribute: type} error:&error];
		ok = wrapper != nil && [wrapper writeToURL:url options:NSFileWrapperWritingAtomic originalContentsURL:nil error:&error];
	} else {
		ok = [self.textView.string writeToURL:url atomically:YES encoding:NSUTF8StringEncoding error:&error];
	}
	if (!ok) {
		viewer_send(self.viewerId, EVENT_FAILED, error.localizedDescription ?: @"Couldn't save");
		return;
	}
	[self setDirtyState:NO];
}

// Editing keys for what's in the viewer: the menu bar's Copy, Paste and
// the rest work on the terminal.
- (BOOL)handleKey:(NSEvent*)event
{
	NSEventModifierFlags flags = event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask;
	if ((flags & NSEventModifierFlagCommand) == 0
		|| (flags & (NSEventModifierFlagControl | NSEventModifierFlagOption)) != 0)
		return NO;
	BOOL shift = (flags & NSEventModifierFlagShift) != 0;
	NSString* key = event.charactersIgnoringModifiers.lowercaseString;
	SEL action = NULL;
	if ([key isEqual:@"s"] && !shift && self.textView != nil) {
		[self save];
		return YES;
	} else if ([key isEqual:@"f"] && !shift && self.textView != nil) {
		NSMenuItem* item = [[NSMenuItem alloc] init];
		item.tag = NSTextFinderActionShowFindInterface;
		[self.textView performTextFinderAction:item];
		return YES;
	} else if ([key isEqual:@"r"] && !shift && self.webView != nil) {
		[self.webView reload];
		return YES;
	} else if ([key isEqual:@"c"] && !shift)
		action = @selector(copy:);
	else if ([key isEqual:@"x"] && !shift)
		action = @selector(cut:);
	else if ([key isEqual:@"v"] && !shift)
		action = @selector(paste:);
	else if ([key isEqual:@"a"] && !shift)
		action = @selector(selectAll:);
	else if ([key isEqual:@"z"])
		action = shift ? @selector(redo:) : @selector(undo:);
	if (action == NULL)
		return NO;
	[NSApp sendAction:action to:nil from:self];
	return YES;
}

- (void)textDidChange:(NSNotification*)note
{
	[self setDirtyState:YES];
}

- (void)observeValueForKeyPath:(NSString*)keyPath ofObject:(id)object change:(NSDictionary*)change context:(void*)context
{
	if ([keyPath isEqual:@"title"]) {
		NSString* title = self.webView.title;
		if (title.length > 0) {
			[self showTitle:title place:nil];
			viewer_send(self.viewerId, EVENT_TITLE, title);
		}
	} else if ([keyPath isEqual:@"URL"]) {
		NSURL* url = self.webView.URL;
		if (url != nil) {
			NSString* address = url.isFileURL ? url.path : url.absoluteString;
			self.path = address;
			[self showTitle:nil place:url.isFileURL ? abbreviated(address) : address];
			viewer_send(self.viewerId, EVENT_URL, address);
		}
	}
}

- (void)webView:(WKWebView*)web didFailProvisionalNavigation:(WKNavigation*)navigation withError:(NSError*)error
{
	if (error.code != NSURLErrorCancelled)
		viewer_send(self.viewerId, EVENT_FAILED, error.localizedDescription);
}

// Links that want a new window open here instead.
- (WKWebView*)webView:(WKWebView*)web createWebViewWithConfiguration:(WKWebViewConfiguration*)config
	forNavigationAction:(WKNavigationAction*)action windowFeatures:(WKWindowFeatures*)features
{
	[web loadRequest:action.request];
	return nil;
}
@end


static TeminoViewer*
viewer_for(uint64_t id)
{
	return sViewers[@(id)];
}

// The viewer `view` is in, if any.
static TeminoViewer*
viewer_around(NSView* view)
{
	for (; view != nil; view = view.superview) {
		if ([view isKindOfClass:TeminoViewer.class])
			return (TeminoViewer*)view;
	}
	return nil;
}

static TeminoViewer*
focused_viewer(NSWindow* window)
{
	NSResponder* responder = window.firstResponder;
	return [responder isKindOfClass:NSView.class] ? viewer_around((NSView*)responder) : nil;
}

static void
viewer_take_focus(TeminoViewer* viewer)
{
	if (viewer.hidden) {
		viewer.pendingFocus = YES;
		return;
	}
	viewer.pendingFocus = NO;
	if (focused_viewer(viewer.window) != viewer)
		[viewer.window makeFirstResponder:viewer.focusView];
}

void
temino_viewer_init(void* view_handle, uint32_t window, TeminoViewerCallback callback)
{
	@autoreleasepool {
		NSView* view = (__bridge NSView*)view_handle;
		TeminoViewerHost* host = [[TeminoViewerHost alloc] initWithFrame:view.bounds];
		host.wantsLayer = YES;
		host.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
		[view addSubview:host];
		if (view.window != nil)
			temino_round_corners(view.window);
		bool first = sViewerHosts == nil;
		if (first) {
			sViewers = [NSMutableDictionary dictionary];
			sViewerHosts = [NSMutableDictionary dictionary];
		}
		sViewerHosts[@(window)] = host;
		sViewerCallback = callback;
		// The monitors are app-wide: they look at the window an event is in.
		if (!first)
			return;
		// A click into a viewer gives it the pane focus too.
		[NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown
			handler:^NSEvent*(NSEvent* event) {
				NSView* content = event.window.contentView;
				NSView* hit = [content hitTest:[content.superview convertPoint:event.locationInWindow fromView:nil]];
				TeminoViewer* viewer = viewer_around(hit);
				if (viewer != nil)
					viewer_send(viewer.viewerId, EVENT_FOCUSED, nil);
				return event;
			}];
		[NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown handler:^NSEvent*(NSEvent* event) {
			TeminoViewer* viewer = focused_viewer(event.window);
			return viewer != nil && [viewer handleKey:event] ? nil : event;
		}];
	}
}

// Opens `target` in viewer `id`, replacing what it showed; a new one
// slides in when it's first placed.
void
temino_viewer_open(uint64_t id, int32_t kind, const char* target, const TeminoViewerStyle* style)
{
	@autoreleasepool {
		NSView* host = host_for(id);
		if (host == nil)
			return;
		TeminoViewer* old = viewer_for(id);
		NSRect frame = old != nil ? old.frame : NSMakeRect(0, 0, 400, 400);
		BOOL focused = old != nil && focused_viewer(host.window) == old;
		[old removeFromSuperview];
		TeminoViewer* viewer = [[TeminoViewer alloc] initWithFrame:frame];
		viewer.viewerId = id;
		viewer.kind = kind;
		viewer.hidden = old != nil ? old.hidden : YES;
		viewer.slideIn = old == nil;
		viewer.wantsLayer = YES;
		viewer.layer.backgroundColor = style_color(style->background).CGColor;
		viewer.appearance = [NSAppearance appearanceNamed:style->dark ? NSAppearanceNameDarkAqua : NSAppearanceNameAqua];
		viewer.path = [NSString stringWithUTF8String:target] ?: @"";
		sViewers[@(id)] = viewer;
		[host addSubview:viewer];
		[viewer buildWithStyle:style];
		switch (kind) {
		case KIND_TEXT:
			[viewer openText:style];
			break;
		case KIND_WEB:
			[viewer openWeb];
			break;
		default:
			[viewer openImage:style];
			break;
		}
		if (focused)
			viewer_take_focus(viewer);
	}
}

void
temino_viewer_place(uint64_t id, double x, double y, double width, double height, bool visible)
{
	TeminoViewer* viewer = viewer_for(id);
	if (viewer == nil)
		return;
	NSRect frame = NSMakeRect(x, y, width, height);
	if (visible && viewer.slideIn) {
		// In from the window's right edge, slowing as it lands.
		viewer.slideIn = NO;
		[CATransaction begin];
		[CATransaction setDisableActions:YES];
		viewer.frame = NSOffsetRect(frame, MAX(NSWidth(viewer.superview.bounds) - x, width), 0);
		viewer.hidden = NO;
		[CATransaction commit];
		[NSAnimationContext runAnimationGroup:^(NSAnimationContext* context) {
			context.duration = kViewerSlide;
			context.timingFunction = [CAMediaTimingFunction functionWithControlPoints:0.2:0.9:0.25:1];
			context.allowsImplicitAnimation = YES;
			viewer.animator.frame = frame;
		}];
	} else {
		[CATransaction begin];
		[CATransaction setDisableActions:YES];
		viewer.frame = frame;
		viewer.hidden = !visible;
		[CATransaction commit];
	}
	if (visible && viewer.pendingFocus)
		viewer_take_focus(viewer);
}

void
temino_viewer_close(uint64_t id)
{
	TeminoViewer* viewer = viewer_for(id);
	if (viewer == nil)
		return;
	NSWindow* window = viewer.window;
	if (focused_viewer(window) == viewer)
		[window makeFirstResponder:viewer.superview.superview];
	[viewer removeFromSuperview];
	[sViewers removeObjectForKey:@(id)];
}

// Gives the keyboard to viewer `id`, or (its pane part 0) back to its
// window's terminal.
void
temino_viewer_focus(uint64_t id)
{
	NSView* host = host_for(id);
	NSWindow* window = host.window;
	TeminoViewer* viewer = viewer_for(id);
	for (TeminoViewer* other in sViewers.allValues) {
		if (other != viewer && other.window == window)
			other.pendingFocus = NO;
	}
	if (viewer != nil)
		viewer_take_focus(viewer);
	else if (window != nil && focused_viewer(window) != nil)
		[window makeFirstResponder:host.superview];
}

// Window `window` closed: its viewers and their host go with it.
void
temino_viewer_detach(uint32_t window)
{
	for (NSNumber* id in sViewers.allKeys)
		if ((uint32_t)(id.unsignedLongLongValue >> 32) == window)
			[sViewers removeObjectForKey:id];
	[sViewerHosts removeObjectForKey:@(window)];
}
