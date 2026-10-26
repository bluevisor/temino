// macOS: the Metal renderer and the menu bar, behind a small C interface
// for src/metal.rs and src/platform/desktop.rs. Like the Vulkan path, each
// frame renders offscreen and comes back as BGRA rows for the window.

#import <AppKit/AppKit.h>
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>
#import <objc/runtime.h>

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
	id<MTLTexture> target;
	id<MTLBuffer> readback;
	id<MTLBuffer> rects;
	id<MTLBuffer> glyphs;
	BOOL atlasReady;
}
@end

@implementation TeminoMetal
@end


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
temino_metal_new(const char* shaders, uint32_t atlas_size, char* name, size_t name_length,
	char* error, size_t error_length)
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
// glyph, glyph count) and returns the frame as tightly packed BGRA rows,
// valid until the next call.
const uint8_t*
temino_metal_render(void* handle, uint32_t width, uint32_t height,
	const float* clear, const uint8_t* atlas_pixels, uint32_t atlas_size,
	int64_t dirty_lo, int64_t dirty_hi,
	const void* rects, size_t rect_size, size_t rect_count,
	const void* glyphs, size_t glyph_size, size_t glyph_count,
	const uint32_t* ranges, size_t layer_count,
	char* error, size_t error_length)
{
	@autoreleasepool {
		TeminoMetal* gpu = (__bridge TeminoMetal*)handle;
		id<MTLDevice> device = gpu->device;

		if (dirty_lo >= 0 && dirty_hi > dirty_lo) {
			MTLRegion region = MTLRegionMake2D(0, (NSUInteger)dirty_lo, atlas_size,
				(NSUInteger)(dirty_hi - dirty_lo));
			[gpu->atlas replaceRegion:region mipmapLevel:0
				withBytes:atlas_pixels + dirty_lo * atlas_size bytesPerRow:atlas_size];
			gpu->atlasReady = YES;
		}

		if (gpu->target == nil || gpu->target.width != width || gpu->target.height != height) {
			MTLTextureDescriptor* desc = [MTLTextureDescriptor
				texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
				width:width height:height mipmapped:NO];
			desc.usage = MTLTextureUsageRenderTarget;
			desc.storageMode = MTLStorageModePrivate;
			gpu->target = [device newTextureWithDescriptor:desc];
			gpu->readback = [device newBufferWithLength:(NSUInteger)width * height * 4
				options:MTLResourceStorageModeShared];
			if (gpu->target == nil || gpu->readback == nil) {
				set_error(error, error_length, @"allocating the frame");
				return NULL;
			}
		}
		gpu->rects = fit_buffer(device, gpu->rects, rects, rect_size * rect_count);
		gpu->glyphs = fit_buffer(device, gpu->glyphs, glyphs, glyph_size * glyph_count);

		id<MTLCommandBuffer> command = [gpu->queue commandBuffer];
		MTLRenderPassDescriptor* pass = [MTLRenderPassDescriptor renderPassDescriptor];
		pass.colorAttachments[0].texture = gpu->target;
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
				[encoder drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4
					instanceCount:range[3]];
			}
		}
		[encoder endEncoding];

		id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
		[blit copyFromTexture:gpu->target sourceSlice:0 sourceLevel:0
			sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(width, height, 1)
			toBuffer:gpu->readback destinationOffset:0
			destinationBytesPerRow:(NSUInteger)width * 4
			destinationBytesPerImage:(NSUInteger)width * height * 4];
		[blit endEncoding];
		[command commit];
		[command waitUntilCompleted];
		if (command.status == MTLCommandBufferStatusError) {
			set_error(error, error_length, [NSString stringWithFormat:@"rendering: %@", command.error]);
			return NULL;
		}
		return (const uint8_t*)gpu->readback.contents;
	}
}


// MARK: Menu bar

// Rust lists the items; picking one calls back with its tag. An item
// tagged kChoiceTag or a multiple of it holds a submenu of choices, whose
// entries are tagged from there up.
typedef struct {
	const char* menu;   // the top-level menu it goes in
	const char* label;  // NULL for a separator
	const char* key;    // key equivalent, "" for none
	uint32_t modifiers; // NSEventModifierFlags
	int32_t tag;
} TeminoMenuItem;

typedef void (*TeminoMenuCallback)(int32_t tag);

static const int32_t kChoiceTag = 1000;
static TeminoMenuCallback sMenuCallback;
static NSMutableDictionary<NSNumber*, NSMenu*>* sChoiceMenus;


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
static NSMenu* sSettingsMenu;


@interface TeminoSettingsTarget : NSObject
- (void)showSettings:(id)sender;
@end

@implementation TeminoSettingsTarget
// Settings are menus of choices; Command+, pops them up at the top left
// of the window, the way a settings window would open there.
- (void)showSettings:(id)sender
{
	NSWindow* window = NSApp.keyWindow ?: NSApp.mainWindow;
	if (sSettingsMenu == nil || window == nil)
		return;
	NSRect content = [window contentRectForFrameRect:window.frame];
	NSPoint at = NSMakePoint(NSMinX(content) + 12, NSMaxY(content) - 8);
	[sSettingsMenu popUpMenuPositioningItem:nil atLocation:at inView:nil];
}
@end

static TeminoSettingsTarget* sSettingsTarget;


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
		sChoiceMenus = [NSMutableDictionary new];
		// Temino has its own tabs; AppKit's would add a second set of items.
		NSWindow.allowsAutomaticWindowTabbing = NO;
		NSString* name = @"Temino";
		NSMenu* bar = [NSMenu new];

		// The application menu: AppKit's own items, then Rust's.
		NSMenu* app = top_menu(bar, name);
		add_item(app, [@"About " stringByAppendingString:name],
			@selector(orderFrontStandardAboutPanel:), @"", 0);
		[app addItem:[NSMenuItem separatorItem]];
		sSettingsTarget = [TeminoSettingsTarget new];
		add_item(app, @"Settings…", @selector(showSettings:), @",", NSEventModifierFlagCommand).target =
			sSettingsTarget;
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
			if (items[i].tag >= kChoiceTag) {
				// The submenu the choices go in.
				NSMenuItem* holder = [menu addItemWithTitle:@(items[i].label) action:nil keyEquivalent:@""];
				holder.submenu = [[NSMenu alloc] initWithTitle:@(items[i].label)];
				sChoiceMenus[@(items[i].tag)] = holder.submenu;
				continue;
			}
			NSMenuItem* item = add_item(menu, @(items[i].label), @selector(pick:),
				@(items[i].key), items[i].modifiers);
			item.target = sMenuTarget;
			item.tag = items[i].tag;
		}

		sSettingsMenu = top_menu(bar, @"Settings");
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


// Rust names the choices of the submenu tagged `tag` and which is picked
// (the app refills the menu after each pick).
void
temino_menu_choices(int32_t tag, const char* const* names, size_t count, size_t selected)
{
	@autoreleasepool {
		NSMenu* menu = sChoiceMenus[@(tag)];
		if (menu == nil)
			return;
		[menu removeAllItems];
		for (size_t i = 0; i < count; i++) {
			NSMenuItem* item = [menu addItemWithTitle:@(names[i])
				action:@selector(pick:) keyEquivalent:@""];
			item.target = sMenuTarget;
			item.tag = tag + (NSInteger)i;
			item.state = i == selected ? NSControlStateValueOn : NSControlStateValueOff;
		}
	}
}


// MARK: Presenting

// Shows a frame of premultiplied BGRA pixels in the view through a layer
// of its own, keeping alpha so a see-through background shows the desktop
// (softbuffer, used elsewhere, draws every pixel opaque).
void
temino_layer_present(void* view_handle, const uint32_t* pixels, uint32_t width, uint32_t height)
{
	@autoreleasepool {
		NSView* view = (__bridge NSView*)view_handle;
		static const void* kLayerKey = &kLayerKey;
		CALayer* layer = objc_getAssociatedObject(view, kLayerKey);
		if (layer == nil) {
			view.wantsLayer = YES;
			layer = [CALayer layer];
			layer.anchorPoint = CGPointZero;
			layer.geometryFlipped = YES;
			layer.contentsGravity = kCAGravityTopLeft;
			[view.layer addSublayer:layer];
			objc_setAssociatedObject(view, kLayerKey, layer, OBJC_ASSOCIATION_RETAIN);
		}
		NSData* data = [NSData dataWithBytes:pixels length:(NSUInteger)width * height * 4];
		CGDataProviderRef provider = CGDataProviderCreateWithCFData((__bridge CFDataRef)data);
		CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
		CGImageRef image = CGImageCreate(width, height, 8, 32, (size_t)width * 4, space,
			kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little, provider, NULL, false,
			kCGRenderingIntentDefault);
		CGColorSpaceRelease(space);
		CGDataProviderRelease(provider);
		[CATransaction begin];
		[CATransaction setDisableActions:YES];
		layer.frame = view.layer.bounds;
		layer.contentsScale = view.window.backingScaleFactor ?: 1.0;
		layer.contents = (__bridge id)image;
		[CATransaction commit];
		CGImageRelease(image);
	}
}
