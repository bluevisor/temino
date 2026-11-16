// macOS: Temino as the system's terminal (src/opener.rs). Files and links
// other apps open in Temino arrive as Apple events; Launch Services keeps
// which app opens scripts and ssh://, telnet:// and x-man-page:// links.

#import <AppKit/AppKit.h>
#import <CoreServices/CoreServices.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>

#include <stdbool.h>
#include <stdint.h>

// kind 0: a link; 1: a file's path.
typedef void (*TeminoOpenCallback)(int32_t kind, const char* text);
static TeminoOpenCallback sOpened;

@interface TeminoOpener : NSObject
@end

@implementation TeminoOpener

- (void)openURL:(NSAppleEventDescriptor*)event reply:(NSAppleEventDescriptor*)reply
{
	(void)reply;
	NSString* url = [event paramDescriptorForKeyword:keyDirectObject].stringValue;
	if (url != nil && sOpened != NULL)
		sOpened(0, url.UTF8String);
}

// One file, or a list of them; each a file URL.
- (void)openDocuments:(NSAppleEventDescriptor*)event reply:(NSAppleEventDescriptor*)reply
{
	(void)reply;
	NSAppleEventDescriptor* direct = [event paramDescriptorForKeyword:keyDirectObject];
	NSMutableArray<NSAppleEventDescriptor*>* items = [NSMutableArray array];
	if (direct.descriptorType == typeAEList) {
		for (NSInteger i = 1; i <= direct.numberOfItems; i++)
			[items addObject:[direct descriptorAtIndex:i]];
	} else if (direct != nil) {
		[items addObject:direct];
	}
	for (NSAppleEventDescriptor* item in items) {
		NSAppleEventDescriptor* file = [item coerceToDescriptorType:typeFileURL];
		NSString* text = file ? [[NSString alloc] initWithData:file.data encoding:NSUTF8StringEncoding] : nil;
		NSURL* url = text ? [NSURL URLWithString:text] : nil;
		if (url.fileURL && sOpened != NULL)
			sOpened(1, url.path.UTF8String);
	}
}

@end

static TeminoOpener* sOpener;

static void
install_handlers(void)
{
	NSAppleEventManager* events = NSAppleEventManager.sharedAppleEventManager;
	[events setEventHandler:sOpener
				andSelector:@selector(openURL:reply:)
			  forEventClass:kInternetEventClass
				 andEventID:kAEGetURL];
	[events setEventHandler:sOpener
				andSelector:@selector(openDocuments:reply:)
			  forEventClass:kCoreEventClass
				 andEventID:kAEOpenDocuments];
}

// Called before the event loop runs. AppKit installs its own handlers as it
// finishes launching, and only then sends the event Temino was launched
// for: so ours go in on its will-finish notification, replacing AppKit's.
void
temino_opener_install(TeminoOpenCallback callback)
{
	sOpened = callback;
	sOpener = [TeminoOpener new];
	install_handlers();
	[NSNotificationCenter.defaultCenter addObserverForName:NSApplicationWillFinishLaunchingNotification
													object:nil
													 queue:nil
												usingBlock:^(NSNotification* note) {
													(void)note;
													install_handlers();
												}];
}

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

// What double-clicking opens: executables, and Terminal's .command/.tool.
static CFStringRef const kTypes[] = { CFSTR("public.unix-executable"), CFSTR("com.apple.terminal.shell-script") };
static CFStringRef const kSchemes[] = { CFSTR("ssh"), CFSTR("telnet"), CFSTR("x-man-page") };

static NSString*
own_id(void)
{
	NSString* id = NSBundle.mainBundle.bundleIdentifier;
	return id != nil ? id : @"com.bluevisor.temino";
}

// Whether double-clicking an executable opens Temino.
bool
temino_default_terminal_is(void)
{
	CFStringRef handler = LSCopyDefaultRoleHandlerForContentType(kTypes[0], kLSRolesShell);
	bool ours = handler != NULL && [(__bridge NSString*)handler caseInsensitiveCompare:own_id()] == NSOrderedSame;
	if (handler != NULL)
		CFRelease(handler);
	return ours;
}

// Content types handed back to Terminal one after another, through the
// API that asks the user first: Launch Services' own call returns success
// but leaves Temino in place (macOS 26).
static void
give_back(NSURL* terminal, NSUInteger next) API_AVAILABLE(macos(12.0))
{
	if (next >= sizeof kTypes / sizeof kTypes[0])
		return;
	UTType* type = [UTType typeWithIdentifier:(__bridge NSString*)kTypes[next]];
	if (type == nil)
		return give_back(terminal, next + 1);
	[NSWorkspace.sharedWorkspace setDefaultApplicationAtURL:terminal
										  toOpenContentType:type
										  completionHandler:^(NSError* error) {
											  if (error != nil)
												  NSLog(@"Temino: Terminal back for %@: %@", type.identifier, error);
											  dispatch_async(dispatch_get_main_queue(), ^{
												  give_back(terminal, next + 1);
											  });
										  }];
}

// Temino (on) or Terminal opens them all; the first failure's status, or 0.
// Launch Services applies it a few seconds later.
int32_t
temino_default_terminal_set(bool on)
{
	CFStringRef handler = (__bridge CFStringRef)(on ? own_id() : @"com.apple.Terminal");
	OSStatus first = noErr;
	for (size_t i = 0; i < sizeof kSchemes / sizeof kSchemes[0]; i++) {
		OSStatus status = LSSetDefaultHandlerForURLScheme(kSchemes[i], handler);
		if (first == noErr)
			first = status;
	}
	if (!on) {
		if (@available(macOS 12.0, *)) {
			NSURL* terminal = [NSWorkspace.sharedWorkspace URLForApplicationWithBundleIdentifier:@"com.apple.Terminal"];
			if (terminal != nil) {
				dispatch_async(dispatch_get_main_queue(), ^{
					give_back(terminal, 0);
				});
				return first;
			}
		}
	}
	for (size_t i = 0; i < sizeof kTypes / sizeof kTypes[0]; i++) {
		OSStatus status = LSSetDefaultRoleHandlerForContentType(kTypes[i], kLSRolesShell, handler);
		if (first == noErr)
			first = status;
	}
	return first;
}

#pragma clang diagnostic pop
