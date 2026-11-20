// macOS: desktop notifications from Temino itself (src/notify.rs). Posted
// through UNUserNotificationCenter they're Temino's, so a click comes back
// here and opens the pane they're about; osascript's belong to Script
// Editor, whose Open dialog a click on one brought up.

#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>

#include <stdbool.h>
#include <stdint.h>

// The window (from 1; 0 for none) and pane (UINT64_MAX for none) a
// clicked notification was about.
typedef void (*TeminoNotifyClicked)(uint32_t window, uint64_t pane);
static TeminoNotifyClicked sClicked;

// How long to wait for the notification center's answers; for the first
// notification's permission, the user's.
static const int64_t kAnswer = 5 * NSEC_PER_SEC;
static const int64_t kPermission = 300 * NSEC_PER_SEC;

// Only the app bundle may use the notification center: a bare binary (a dev
// build, with Info.plist linked in) makes it throw.
static bool
bundled(void)
{
	NSBundle* main = NSBundle.mainBundle;
	return main.bundleIdentifier != nil && [main.bundlePath.pathExtension isEqualToString:@"app"];
}

@interface TeminoNotifier : NSObject <UNUserNotificationCenterDelegate>
@end

@implementation TeminoNotifier

// Temino only notifies about a pane you aren't looking at: show it even
// while Temino is in front.
- (void)userNotificationCenter:(UNUserNotificationCenter*)center
	   willPresentNotification:(UNNotification*)notification
		 withCompletionHandler:(void (^)(UNNotificationPresentationOptions))done
{
	(void)center;
	(void)notification;
	done(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList
		 | UNNotificationPresentationOptionSound);
}

// A click: Temino comes forward, at the pane the notification was about.
- (void)userNotificationCenter:(UNUserNotificationCenter*)center
	didReceiveNotificationResponse:(UNNotificationResponse*)response
			 withCompletionHandler:(void (^)(void))done
{
	(void)center;
	NSDictionary* info = response.notification.request.content.userInfo;
	NSNumber* window = [info[@"window"] isKindOfClass:NSNumber.class] ? info[@"window"] : nil;
	NSNumber* pane = [info[@"pane"] isKindOfClass:NSNumber.class] ? info[@"pane"] : nil;
	bool clicked = [response.actionIdentifier isEqualToString:UNNotificationDefaultActionIdentifier];
	dispatch_async(dispatch_get_main_queue(), ^{
		if (!clicked)
			return;
		[NSApp unhide:nil];
		[NSApp activateIgnoringOtherApps:YES];
		if (sClicked != NULL)
			sClicked(window.unsignedIntValue, pane != nil ? pane.unsignedLongLongValue : UINT64_MAX);
	});
	done();
}

@end

static TeminoNotifier* sNotifier;

// Before the event loop runs, so a click that launches Temino isn't missed.
void
temino_notify_install(TeminoNotifyClicked clicked)
{
	sClicked = clicked;
	if (!bundled())
		return;
	sNotifier = [TeminoNotifier new];
	UNUserNotificationCenter.currentNotificationCenter.delegate = sNotifier;
}

// Whether Temino may notify: asks you the first time (macOS shows its own
// prompt), and waits for the answer. 1 yes, 0 no, -1 no answer yet.
static int
allowed(UNUserNotificationCenter* center)
{
	dispatch_semaphore_t got = dispatch_semaphore_create(0);
	__block UNAuthorizationStatus status = UNAuthorizationStatusNotDetermined;
	[center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings* settings) {
		status = settings.authorizationStatus;
		dispatch_semaphore_signal(got);
	}];
	if (dispatch_semaphore_wait(got, dispatch_time(DISPATCH_TIME_NOW, kAnswer)) != 0)
		return -1;
	if (status == UNAuthorizationStatusAuthorized || status == UNAuthorizationStatusProvisional)
		return 1;
	if (status != UNAuthorizationStatusNotDetermined)
		return 0;
	__block BOOL granted = NO;
	[center requestAuthorizationWithOptions:UNAuthorizationOptionAlert | UNAuthorizationOptionSound
						  completionHandler:^(BOOL yes, NSError* error) {
							  (void)error;
							  granted = yes;
							  dispatch_semaphore_signal(got);
						  }];
	if (dispatch_semaphore_wait(got, dispatch_time(DISPATCH_TIME_NOW, kPermission)) != 0)
		return -1;
	return granted ? 1 : 0;
}

// Shows a notification from Temino, about `pane` of `window` (see
// TeminoNotifyClicked). 1 shown (or dropped while macOS still asks you);
// 0 when Temino can't post its own (not the app bundle, not allowed, an
// error): the caller shows it another way. Waits: call it off the main
// thread.
int32_t
temino_notify_post(const char* title, const char* body, uint32_t window, uint64_t pane)
{
	if (!bundled())
		return 0;
	UNUserNotificationCenter* center = UNUserNotificationCenter.currentNotificationCenter;
	int allow = allowed(center);
	if (allow <= 0)
		return allow < 0 ? 1 : 0;
	UNMutableNotificationContent* content = [UNMutableNotificationContent new];
	content.title = [NSString stringWithUTF8String:title] ?: @"Temino";
	content.body = [NSString stringWithUTF8String:body] ?: @"";
	content.sound = UNNotificationSound.defaultSound;
	content.userInfo = @{ @"window" : @(window), @"pane" : @(pane) };
	UNNotificationRequest* request = [UNNotificationRequest requestWithIdentifier:NSUUID.UUID.UUIDString
																		  content:content
																		  trigger:nil];
	dispatch_semaphore_t done = dispatch_semaphore_create(0);
	__block bool posted = false;
	[center addNotificationRequest:request
			 withCompletionHandler:^(NSError* error) {
				 posted = error == nil;
				 dispatch_semaphore_signal(done);
			 }];
	// Still on its way: it'll show; don't show it twice.
	if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, kAnswer)) != 0)
		return 1;
	return posted ? 1 : 0;
}
