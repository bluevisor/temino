// macOS: the Metal renderer and the menu bar, behind a small C interface
// for src/metal.rs and src/platform/desktop.rs. Frames render straight into
// the window's CAMetalLayer; before there is one (and for --selftest, or
// TEMINO_CAPTURE) they also come back as BGRA rows.

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


// MARK: Metal

@interface TeminoMetal : NSObject {
@public
	id<MTLDevice> device;
	id<MTLCommandQueue> queue;
	id<MTLRenderPipelineState> rectPipeline;
	id<MTLRenderPipelineState> glyphPipeline;
	id<MTLTexture> atlas;
	id<MTLTexture> colorAtlas;
	id<MTLTexture> target;
	id<MTLBuffer> readback;
	id<MTLBuffer> rects;
	id<MTLBuffer> glyphs;
	BOOL atlasReady;
}
@end

@implementation TeminoMetal
@end


// Draws `text` (an emoji, with any modifiers and joiners) in Apple Color
// Emoji at `size` pixels, shrunk to fit max_width x max_height, into `out`
// as premultiplied RGBA rows (up to `capacity` bytes). False when the font
// has no emoji for it.
bool
temino_emoji(const char* text, float size, float max_width, float max_height,
	uint8_t* out, size_t capacity, uint32_t* width, uint32_t* height)
{
	@autoreleasepool {
		NSString* string = @(text);
		if (string.length == 0)
			return false;
		CTLineRef line = NULL;
		double ascent = 0, descent = 0, advance = 0;
		for (int attempt = 0; attempt < 2; attempt++) {
			CTFontRef font = CTFontCreateWithName(CFSTR("AppleColorEmoji"), size, NULL);
			NSAttributedString* attributed = [[NSAttributedString alloc] initWithString:string
				attributes:@{ (__bridge NSString*)kCTFontAttributeName: (__bridge id)font }];
			CFRelease(font);
			if (line != NULL)
				CFRelease(line);
			line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)attributed);
			advance = CTLineGetTypographicBounds(line, &ascent, &descent, NULL);
			double fit = fmin(max_width / fmax(advance, 1), max_height / fmax(ascent + descent, 1));
			if (fit >= 1)
				break;
			size = floor(size * fit);
		}
		// Every run must have come from the emoji font, not a substitute.
		bool emoji = advance > 0;
		NSArray* runs = (__bridge NSArray*)CTLineGetGlyphRuns(line);
		for (id run in runs) {
			CTFontRef used = (CTFontRef)CFDictionaryGetValue(
				CTRunGetAttributes((__bridge CTRunRef)run), kCTFontAttributeName);
			NSString* name = CFBridgingRelease(CTFontCopyPostScriptName(used));
			if (![name isEqualToString:@"AppleColorEmoji"])
				emoji = false;
		}
		size_t w = (size_t)ceil(advance), h = (size_t)ceil(ascent + descent);
		if (!emoji || w == 0 || h == 0 || w * h * 4 > capacity) {
			CFRelease(line);
			return false;
		}
		memset(out, 0, w * h * 4);
		CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
		CGContextRef context = CGBitmapContextCreate(out, w, h, 8, w * 4, space,
			kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
		CGColorSpaceRelease(space);
		CGContextSetTextPosition(context, (w - advance) / 2, descent);
		CTLineDraw(line, context);
		CGContextRelease(context);
		CFRelease(line);
		*width = (uint32_t)w;
		*height = (uint32_t)h;
		return true;
	}
}


// The window's layer, attached on the main thread and drawn into from the
// core thread (retained; there is one window).
static _Atomic(void*) sLayer;


static void
set_error(char* error, size_t length, NSString* text)
{
	if (error != NULL && length > 0)
		strlcpy(error, text.UTF8String ?: "unknown error", length);
}


static id<MTLRenderPipelineState>
make_pipeline(id<MTLDevice> device, id<MTLLibrary> library, NSString* vertex,
	NSString* fragment, NSError** error)
{
	MTLRenderPipelineDescriptor* desc = [MTLRenderPipelineDescriptor new];
	desc.vertexFunction = [library newFunctionWithName:vertex];
	desc.fragmentFunction = [library newFunctionWithName:fragment];
	MTLRenderPipelineColorAttachmentDescriptor* color = desc.colorAttachments[0];
	color.pixelFormat = MTLPixelFormatBGRA8Unorm;
	// Premultiplied "over", as the Vulkan pipelines blend.
	color.blendingEnabled = YES;
	color.sourceRGBBlendFactor = MTLBlendFactorOne;
	color.destinationRGBBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
	color.sourceAlphaBlendFactor = MTLBlendFactorOne;
	color.destinationAlphaBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
	return [device newRenderPipelineStateWithDescriptor:desc error:error];
}


void*
temino_metal_new(const char* shaders, uint32_t atlas_size, uint32_t color_atlas_size,
	char* name, size_t name_length, char* error, size_t error_length)
{
	@autoreleasepool {
		TeminoMetal* gpu = [TeminoMetal new];
		gpu->device = MTLCreateSystemDefaultDevice();
		if (gpu->device == nil) {
			set_error(error, error_length, @"no Metal device");
			return NULL;
		}
		gpu->queue = [gpu->device newCommandQueue];
		NSError* failure = nil;
		id<MTLLibrary> library = [gpu->device newLibraryWithSource:@(shaders) options:nil error:&failure];
		if (library == nil) {
			set_error(error, error_length, [NSString stringWithFormat:@"compiling shaders: %@", failure]);
			return NULL;
		}
		gpu->rectPipeline = make_pipeline(gpu->device, library, @"rectVertex", @"rectFragment", &failure);
		gpu->glyphPipeline = make_pipeline(gpu->device, library, @"glyphVertex", @"glyphFragment", &failure);
		if (gpu->rectPipeline == nil || gpu->glyphPipeline == nil) {
			set_error(error, error_length, [NSString stringWithFormat:@"creating pipelines: %@", failure]);
			return NULL;
		}
		MTLTextureDescriptor* desc = [MTLTextureDescriptor
			texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm
			width:atlas_size height:atlas_size mipmapped:NO];
		desc.usage = MTLTextureUsageShaderRead;
		gpu->atlas = [gpu->device newTextureWithDescriptor:desc];
		desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm
			width:color_atlas_size height:color_atlas_size mipmapped:NO];
		desc.usage = MTLTextureUsageShaderRead;
		gpu->colorAtlas = [gpu->device newTextureWithDescriptor:desc];
		set_error(name, name_length, gpu->device.name);
		return (__bridge_retained void*)gpu;
	}
}


void
temino_metal_free(void* handle)
{
	if (handle != NULL)
		(void)(__bridge_transfer TeminoMetal*)handle;
}


static id<MTLBuffer>
fit_buffer(id<MTLDevice> device, id<MTLBuffer> buffer, const void* data, size_t bytes)
{
	if (bytes == 0)
		return buffer;
	if (buffer == nil || buffer.length < bytes) {
		size_t size = 64 * 1024;
		while (size < bytes)
			size *= 2;
		buffer = [device newBufferWithLength:size options:MTLResourceStorageModeShared];
	}
	memcpy(buffer.contents, data, bytes);
	return buffer;
}


// Renders the layers (each `ranges` entry is first rect, rect count, first
// glyph, glyph count) into the window's layer, and shows it there. With no
// layer yet, or `readback` set, `*pixels` gets the frame as tightly packed
// BGRA rows, valid until the next call; otherwise NULL. False on failure.
bool
temino_metal_render(void* handle, uint32_t width, uint32_t height,
	const float* clear, const uint8_t* atlas_pixels, uint32_t atlas_size,
	int64_t dirty_lo, int64_t dirty_hi,
	const uint8_t* color_pixels, uint32_t color_size, int64_t color_lo, int64_t color_hi,
	const void* rects, size_t rect_size, size_t rect_count,
	const void* glyphs, size_t glyph_size, size_t glyph_count,
	const uint32_t* ranges, size_t layer_count, bool readback,
	const uint8_t** pixels, char* error, size_t error_length)
{
	@autoreleasepool {
		TeminoMetal* gpu = (__bridge TeminoMetal*)handle;
		id<MTLDevice> device = gpu->device;
		*pixels = NULL;

		if (dirty_lo >= 0 && dirty_hi > dirty_lo) {
			MTLRegion region = MTLRegionMake2D(0, (NSUInteger)dirty_lo, atlas_size,
				(NSUInteger)(dirty_hi - dirty_lo));
			[gpu->atlas replaceRegion:region mipmapLevel:0
				withBytes:atlas_pixels + dirty_lo * atlas_size bytesPerRow:atlas_size];
			gpu->atlasReady = YES;
		}
		if (color_lo >= 0 && color_hi > color_lo) {
			MTLRegion region = MTLRegionMake2D(0, (NSUInteger)color_lo, color_size,
				(NSUInteger)(color_hi - color_lo));
			[gpu->colorAtlas replaceRegion:region mipmapLevel:0
				withBytes:color_pixels + color_lo * color_size * 4 bytesPerRow:color_size * 4];
		}

		CAMetalLayer* layer = (__bridge CAMetalLayer*)atomic_load(&sLayer);
		id<CAMetalDrawable> drawable = nil;
		id<MTLTexture> target;
		if (layer != nil) {
			// Off the main thread, layer changes need a transaction of their own.
			CGSize size = CGSizeMake(width, height);
			if (layer.device != device || !CGSizeEqualToSize(layer.drawableSize, size)) {
				[CATransaction begin];
				[CATransaction setDisableActions:YES];
				layer.device = device;
				layer.drawableSize = size;
				[CATransaction commit];
			}
			drawable = [layer nextDrawable];
			if (drawable == nil)
				return true; // none free in time: this frame is skipped
			target = drawable.texture;
		} else {
			if (gpu->target == nil || gpu->target.width != width || gpu->target.height != height) {
				MTLTextureDescriptor* desc = [MTLTextureDescriptor
					texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
					width:width height:height mipmapped:NO];
				desc.usage = MTLTextureUsageRenderTarget;
				desc.storageMode = MTLStorageModePrivate;
				gpu->target = [device newTextureWithDescriptor:desc];
			}
			target = gpu->target;
			readback = true;
		}
		NSUInteger frame_bytes = (NSUInteger)width * height * 4;
		if (readback && (gpu->readback == nil || gpu->readback.length != frame_bytes))
			gpu->readback = [device newBufferWithLength:frame_bytes options:MTLResourceStorageModeShared];
		if (target == nil || (readback && gpu->readback == nil)) {
			set_error(error, error_length, @"allocating the frame");
			return false;
		}
		gpu->rects = fit_buffer(device, gpu->rects, rects, rect_size * rect_count);
		gpu->glyphs = fit_buffer(device, gpu->glyphs, glyphs, glyph_size * glyph_count);

		id<MTLCommandBuffer> command = [gpu->queue commandBuffer];
		MTLRenderPassDescriptor* pass = [MTLRenderPassDescriptor renderPassDescriptor];
		pass.colorAttachments[0].texture = target;
		pass.colorAttachments[0].loadAction = MTLLoadActionClear;
		pass.colorAttachments[0].storeAction = MTLStoreActionStore;
		pass.colorAttachments[0].clearColor = MTLClearColorMake(clear[0], clear[1], clear[2], clear[3]);
		id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
		float viewport[2] = { (float)width, (float)height };
		for (size_t i = 0; i < layer_count; i++) {
			const uint32_t* range = ranges + i * 4;
			if (range[1] > 0) {
				[encoder setRenderPipelineState:gpu->rectPipeline];
				[encoder setVertexBuffer:gpu->rects offset:range[0] * rect_size atIndex:0];
				[encoder setVertexBytes:viewport length:sizeof(viewport) atIndex:1];
				[encoder drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4
					instanceCount:range[1]];
			}
			if (range[3] > 0 && gpu->atlasReady) {
				[encoder setRenderPipelineState:gpu->glyphPipeline];
				[encoder setVertexBuffer:gpu->glyphs offset:range[2] * glyph_size atIndex:0];
				[encoder setVertexBytes:viewport length:sizeof(viewport) atIndex:1];
				[encoder setFragmentTexture:gpu->atlas atIndex:0];
				[encoder setFragmentTexture:gpu->colorAtlas atIndex:1];
				[encoder drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4
					instanceCount:range[3]];
			}
		}
		[encoder endEncoding];

		if (readback) {
			id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
			[blit copyFromTexture:target sourceSlice:0 sourceLevel:0
				sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(width, height, 1)
				toBuffer:gpu->readback destinationOffset:0
				destinationBytesPerRow:(NSUInteger)width * 4
				destinationBytesPerImage:frame_bytes];
			[blit endEncoding];
		}
		if (drawable != nil)
			[command presentDrawable:drawable];
		[command commit];
		// The instance buffers are refilled next frame: let this one finish.
		[command waitUntilCompleted];
		if (command.status == MTLCommandBufferStatusError) {
			set_error(error, error_length, [NSString stringWithFormat:@"rendering: %@", command.error]);
			return false;
		}
		if (readback)
			*pixels = (const uint8_t*)gpu->readback.contents;
		return true;
	}
}


// MARK: Menu bar

// Rust lists the items; picking one calls back with its tag.
typedef struct {
	const char* menu;   // the top-level menu it goes in
	const char* label;  // NULL for a separator
	const char* key;    // key equivalent, "" for none
	uint32_t modifiers; // NSEventModifierFlags
	int32_t tag;
} TeminoMenuItem;

typedef void (*TeminoMenuCallback)(int32_t tag);

static TeminoMenuCallback sMenuCallback;


@interface TeminoMenuTarget : NSObject
- (void)pick:(NSMenuItem*)item;
@end

@implementation TeminoMenuTarget
- (void)pick:(NSMenuItem*)item
{
	if (sMenuCallback != NULL)
		sMenuCallback((int32_t)item.tag);
}
@end

static TeminoMenuTarget* sMenuTarget;


static NSMenu*
top_menu(NSMenu* bar, NSString* title)
{
	for (NSMenuItem* item in bar.itemArray) {
		if ([item.submenu.title isEqualToString:title])
			return item.submenu;
	}
	NSMenuItem* holder = [NSMenuItem new];
	NSMenu* menu = [[NSMenu alloc] initWithTitle:title];
	holder.submenu = menu;
	[bar addItem:holder];
	return menu;
}


static NSMenuItem*
add_item(NSMenu* menu, NSString* title, SEL action, NSString* key, NSEventModifierFlags modifiers)
{
	NSMenuItem* item = [menu addItemWithTitle:title action:action keyEquivalent:key];
	item.keyEquivalentModifierMask = modifiers;
	return item;
}


void
temino_menu_install(const TeminoMenuItem* items, size_t count, TeminoMenuCallback callback)
{
	@autoreleasepool {
		sMenuCallback = callback;
		sMenuTarget = [TeminoMenuTarget new];
		// Temino has its own tabs; AppKit's would add a second set of items.
		NSWindow.allowsAutomaticWindowTabbing = NO;
		NSString* name = @"Temino";
		NSMenu* bar = [NSMenu new];

		// The application menu: AppKit's own items, then Rust's.
		NSMenu* app = top_menu(bar, name);
		add_item(app, [@"About " stringByAppendingString:name],
			@selector(orderFrontStandardAboutPanel:), @"", 0);
		[app addItem:[NSMenuItem separatorItem]];
		NSMenu* services = [[NSMenu alloc] initWithTitle:@"Services"];
		[app addItemWithTitle:@"Services" action:nil keyEquivalent:@""].submenu = services;
		NSApp.servicesMenu = services;
		[app addItem:[NSMenuItem separatorItem]];
		add_item(app, [@"Hide " stringByAppendingString:name], @selector(hide:), @"h",
			NSEventModifierFlagCommand);
		add_item(app, @"Hide Others", @selector(hideOtherApplications:), @"h",
			NSEventModifierFlagCommand | NSEventModifierFlagOption);
		add_item(app, @"Show All", @selector(unhideAllApplications:), @"", 0);
		[app addItem:[NSMenuItem separatorItem]];

		for (size_t i = 0; i < count; i++) {
			NSMenu* menu = top_menu(bar, @(items[i].menu));
			if (items[i].label == NULL) {
				[menu addItem:[NSMenuItem separatorItem]];
				continue;
			}
			NSMenuItem* item = add_item(menu, @(items[i].label), @selector(pick:),
				@(items[i].key), items[i].modifiers);
			item.target = sMenuTarget;
			item.tag = items[i].tag;
		}

		NSMenu* window = top_menu(bar, @"Window");
		add_item(window, @"Minimize", @selector(performMiniaturize:), @"m", NSEventModifierFlagCommand);
		add_item(window, @"Zoom", @selector(performZoom:), @"", 0);
		add_item(window, @"Enter Full Screen", @selector(toggleFullScreen:), @"f",
			NSEventModifierFlagCommand | NSEventModifierFlagControl);
		[window addItem:[NSMenuItem separatorItem]];
		add_item(window, @"Bring All to Front", @selector(arrangeInFront:), @"", 0);
		NSApp.windowsMenu = window;

		NSApp.mainMenu = bar;
	}
}


// MARK: Presenting

// Holds the window's CAMetalLayer above winit's view, letting every event
// through to it.
@interface TeminoMetalView : NSView
@end

@implementation TeminoMetalView
- (CALayer*)makeBackingLayer
{
	return [CAMetalLayer layer];
}

- (NSView*)hitTest:(NSPoint)point
{
	return nil;
}

// Frames come in device pixels; without this a Retina frame shows at twice
// its size, cropped to the top left.
- (void)viewDidChangeBackingProperties
{
	[super viewDidChangeBackingProperties];
	CGFloat scale = self.window ? self.window.backingScaleFactor : 1;
	[CATransaction begin];
	[CATransaction setDisableActions:YES];
	self.layer.contentsScale = scale;
	[CATransaction commit];
}
@end


// Gives the window `view` a layer that frames render straight into. They
// keep their premultiplied alpha, so a see-through background shows the
// desktop (blurred, if Settings says so).
void
temino_metal_attach(void* view_handle)
{
	@autoreleasepool {
		NSView* view = (__bridge NSView*)view_handle;
		TeminoMetalView* holder = [[TeminoMetalView alloc] initWithFrame:view.bounds];
		holder.wantsLayer = YES;
		holder.layerContentsRedrawPolicy = NSViewLayerContentsRedrawNever;
		holder.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
		[view addSubview:holder];
		CAMetalLayer* layer = (CAMetalLayer*)holder.layer;
		layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
		layer.opaque = NO;
		// Captures copy frames back out of the drawable.
		layer.framebufferOnly = NO;
		CGColorSpaceRef srgb = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
		layer.colorspace = srgb;
		CGColorSpaceRelease(srgb);
		// While the window resizes, the last frame stays put at the top left.
		layer.contentsGravity = kCAGravityTopLeft;
		layer.contentsScale = view.window ? view.window.backingScaleFactor : 1;
		atomic_store(&sLayer, (__bridge_retained void*)layer);
	}
}


// MARK: Title bar

// The title bar is as tall as a browser's tab strip: the window's content
// runs up under a transparent bar that Temino draws itself, and the
// traffic lights are centered in it. AppKit lays them back out at the
// standard height whenever the window resizes, so they're placed again.
static CGFloat sTitlebarHeight = 0;
// Holds the viewers over the frames (see Viewers below).
static NSView* sViewerHost;

// A see-through window isn't clipped to its rounded corners, so the frames
// would fill them out square. The layer is rounded to match the window,
// whose radius grew in macOS 26 and has no public getter.
static void
round_corners(NSWindow* window)
{
	CAMetalLayer* layer = (__bridge CAMetalLayer*)atomic_load(&sLayer);
	if (layer == nil)
		return;
	CGFloat radius = 0;
	if ((window.styleMask & NSWindowStyleMaskFullScreen) == 0) {
		radius = 10;
		SEL getter = NSSelectorFromString(@"_cornerRadius");
		if ([window respondsToSelector:getter])
			radius = ((CGFloat (*)(id, SEL))objc_msgSend)(window, getter);
	}
	layer.cornerRadius = radius;
	layer.cornerCurve = kCACornerCurveContinuous;
	layer.masksToBounds = radius > 0;
	// The frame view behind it draws the window's hairline border, square
	// unless it's rounded too; the shadow follows once it's redone.
	NSView* frame = window.contentView.superview;
	frame.wantsLayer = YES;
	frame.layer.cornerRadius = radius;
	frame.layer.cornerCurve = kCACornerCurveContinuous;
	frame.layer.masksToBounds = radius > 0;
	[window invalidateShadow];
	if (sViewerHost != nil) {
		sViewerHost.layer.cornerRadius = radius;
		sViewerHost.layer.cornerCurve = kCACornerCurveContinuous;
		sViewerHost.layer.masksToBounds = radius > 0;
	}
}

static void
place_traffic_lights(NSWindow* window)
{
	NSButton* close = [window standardWindowButton:NSWindowCloseButton];
	NSView* bar = close.superview;
	NSView* container = bar.superview;
	static bool placing = false;
	if (container == nil || sTitlebarHeight <= 0 || placing
		|| (window.styleMask & NSWindowStyleMaskFullScreen) != 0)
		return;
	placing = true;
	NSRect frame = container.frame;
	frame.size.height = sTitlebarHeight;
	frame.origin.y = NSHeight(window.frame) - sTitlebarHeight;
	container.frame = frame;
	bar.frame = container.bounds;
	NSWindowButton kinds[] = {NSWindowCloseButton, NSWindowMiniaturizeButton, NSWindowZoomButton};
	for (int i = 0; i < 3; i++) {
		NSButton* button = [window standardWindowButton:kinds[i]];
		NSRect place = button.frame;
		place.origin.x = 14 + i * 20;
		place.origin.y = round((sTitlebarHeight - NSHeight(place)) / 2);
		[button setFrameOrigin:place.origin];
	}
	placing = false;
}


void
temino_titlebar(void* view_handle, double height)
{
	@autoreleasepool {
		NSWindow* window = ((__bridge NSView*)view_handle).window;
		if (window == nil)
			return;
		sTitlebarHeight = height;
		place_traffic_lights(window);
		round_corners(window);
		NSNotificationCenter* center = NSNotificationCenter.defaultCenter;
		NSArray* names = @[NSWindowDidResizeNotification, NSWindowDidEndLiveResizeNotification,
			NSWindowDidEnterFullScreenNotification, NSWindowDidExitFullScreenNotification,
			NSWindowDidBecomeKeyNotification,
			NSWindowDidResignKeyNotification];
		for (NSNotificationName name in names) {
			[center addObserverForName:name object:window queue:nil usingBlock:^(NSNotification* note) {
				place_traffic_lights(note.object);
				round_corners(note.object);
			}];
		}
		// AppKit also lays the bar out on its own, at its standard height,
		// with no window notification; its frame changing gives that away.
		NSView* container = [window standardWindowButton:NSWindowCloseButton].superview.superview;
		container.postsFrameChangedNotifications = YES;
		[center addObserverForName:NSViewFrameDidChangeNotification object:container queue:nil
			usingBlock:^(NSNotification* note) {
				place_traffic_lights(((NSView*)note.object).window);
			}];
	}
}


// Whether frames go straight to the window.
bool
temino_metal_on_screen(void)
{
	return atomic_load(&sLayer) != NULL;
}


// ---- Quick terminal: a global hotkey brings the window and hides it ----

static EventHotKeyRef sHotKey;
static bool sHotKeyHandler;

static NSWindow*
main_window(void)
{
	for (NSWindow* window in NSApp.windows)
		if (window.canBecomeMainWindow)
			return window;
	return nil;
}

// Hides Temino when its window is in front, else brings it to this Space.
void
temino_toggle_window(void)
{
	NSWindow* window = main_window();
	if (window == nil)
		return;
	if (NSApp.isActive && window.isKeyWindow && window.isVisible
		&& !window.isMiniaturized) {
		[NSApp hide:nil];
		return;
	}
	[NSApp unhide:nil];
	if (window.isMiniaturized)
		[window deminiaturize:nil];
	[NSApp activateIgnoringOtherApps:YES];
	[window makeKeyAndOrderFront:nil];
}

static OSStatus
hotkey_pressed(EventHandlerCallRef next, EventRef event, void* data)
{
	(void)next;
	(void)event;
	(void)data;
	temino_toggle_window();
	return noErr;
}

// Registers the hotkey (a virtual key code and Carbon modifiers), or none
// with keycode UINT32_MAX. Carbon hotkeys need no accessibility permission.
void
temino_hotkey(uint32_t keycode, uint32_t modifiers)
{
	if (!sHotKeyHandler) {
		EventTypeSpec type = { kEventClassKeyboard, kEventHotKeyPressed };
		InstallApplicationEventHandler(&hotkey_pressed, 1, &type, NULL, NULL);
		sHotKeyHandler = true;
	}
	if (sHotKey != NULL) {
		UnregisterEventHotKey(sHotKey);
		sHotKey = NULL;
	}
	NSWindow* window = main_window();
	if (keycode == UINT32_MAX) {
		window.collectionBehavior &= ~NSWindowCollectionBehaviorMoveToActiveSpace;
		return;
	}
	EventHotKeyID id = { 'tmno', 1 };
	RegisterEventHotKey(keycode, modifiers, id, GetApplicationEventTarget(), 0, &sHotKey);
	// Summoned, the window comes to the Space you're on.
	window.collectionBehavior |= NSWindowCollectionBehaviorMoveToActiveSpace;
}

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
static NSMutableDictionary<NSNumber*, id>* sViewers;

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
@interface TeminoViewerHost : NSView
@end

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
temino_viewer_init(void* view_handle, TeminoViewerCallback callback)
{
	@autoreleasepool {
		NSView* view = (__bridge NSView*)view_handle;
		sViewerCallback = callback;
		sViewers = [NSMutableDictionary dictionary];
		TeminoViewerHost* host = [[TeminoViewerHost alloc] initWithFrame:view.bounds];
		host.wantsLayer = YES;
		host.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
		[view addSubview:host];
		sViewerHost = host;
		if (view.window != nil)
			round_corners(view.window);
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
		if (sViewerHost == nil)
			return;
		TeminoViewer* old = viewer_for(id);
		NSRect frame = old != nil ? old.frame : NSMakeRect(0, 0, 400, 400);
		BOOL focused = old != nil && focused_viewer(sViewerHost.window) == old;
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
		[sViewerHost addSubview:viewer];
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
		viewer.frame = NSOffsetRect(frame, MAX(NSWidth(sViewerHost.bounds) - x, width), 0);
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
		[window makeFirstResponder:sViewerHost.superview];
	[viewer removeFromSuperview];
	[sViewers removeObjectForKey:@(id)];
}

void
temino_viewer_focus(uint64_t id)
{
	NSWindow* window = sViewerHost.window;
	TeminoViewer* viewer = viewer_for(id);
	for (TeminoViewer* other in sViewers.allValues) {
		if (other != viewer)
			other.pendingFocus = NO;
	}
	if (viewer != nil)
		viewer_take_focus(viewer);
	else if (focused_viewer(window) != nil)
		[window makeFirstResponder:sViewerHost.superview];
}
