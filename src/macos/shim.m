// macOS: the Metal renderer and the menu bar, behind a small C interface
// for src/metal.rs and src/platform/desktop.rs. Frames render straight into
// the window's CAMetalLayer; before there is one (and for --selftest, or
// TEMINO_CAPTURE) they also come back as BGRA rows.

#import <AppKit/AppKit.h>
#import <CoreText/CoreText.h>
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>

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

static void
place_traffic_lights(NSWindow* window)
{
	NSButton* close = [window standardWindowButton:NSWindowCloseButton];
	NSView* bar = close.superview;
	NSView* container = bar.superview;
	if (container == nil || sTitlebarHeight <= 0
		|| (window.styleMask & NSWindowStyleMaskFullScreen) != 0)
		return;
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
		NSNotificationCenter* center = NSNotificationCenter.defaultCenter;
		NSArray* names = @[NSWindowDidResizeNotification, NSWindowDidEndLiveResizeNotification,
			NSWindowDidExitFullScreenNotification, NSWindowDidBecomeKeyNotification,
			NSWindowDidResignKeyNotification];
		for (NSNotificationName name in names) {
			[center addObserverForName:name object:window queue:nil usingBlock:^(NSNotification* note) {
				place_traffic_lights(note.object);
			}];
		}
	}
}


// Whether frames go straight to the window.
bool
temino_metal_on_screen(void)
{
	return atomic_load(&sLayer) != NULL;
}
