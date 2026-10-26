// keysend <team> <title-substring> <step>...: activates that team's window
// and posts key presses to it, for driving Temino in tests. Steps:
//   some text        types the text
//   @enter @tab @up @down @left @right @esc @backspace @home @end
//   @ctrl+c @alt+k   (modifiers: ctrl, alt, opt, shift; combine as @ctrl+shift+x)
//   @sleep=500       waits that many milliseconds
// Build on Haiku: g++ -o keysend keysend.cpp -lbe
#include <Application.h>
#include <InterfaceDefs.h>
#include <Message.h>
#include <Messenger.h>
#include <MessengerPrivate.h>
#include <OS.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct Named {
	const char* name;
	int32 key;
	char byte;
};

static const Named kNamed[] = {
	{"enter", 0x47, B_ENTER},
	{"tab", 0x26, B_TAB},
	{"up", 0x57, B_UP_ARROW},
	{"down", 0x62, B_DOWN_ARROW},
	{"left", 0x61, B_LEFT_ARROW},
	{"right", 0x63, B_RIGHT_ARROW},
	{"esc", 0x01, B_ESCAPE},
	{"backspace", 0x1e, B_BACKSPACE},
	{"home", 0x20, B_HOME},
	{"end", 0x35, B_END},
	{"pageup", 0x21, B_PAGE_UP},
	{"pagedown", 0x36, B_PAGE_DOWN},
};


static void
post(BMessenger& window, int32 key, uint32 modifiers, char rawChar, char byte)
{
	char bytes[2] = {byte, 0};
	const uint32 whats[] = {B_KEY_DOWN, B_KEY_UP};
	for (uint32 what : whats) {
		BMessage message(what);
		message.AddInt64("when", system_time());
		message.AddInt32("modifiers", modifiers);
		message.AddInt32("key", key);
		message.AddInt32("raw_char", rawChar);
		message.AddInt8("byte", byte);
		message.AddString("bytes", bytes);
		window.SendMessage(&message);
		snooze(20000);
	}
}


int
main(int argc, char** argv)
{
	if (argc < 4) {
		fprintf(stderr, "usage: keysend <team> <title-substring> <step>...\n");
		return 2;
	}
	BApplication app("application/x-vnd.bluevisor-keysend");
	BMessenger target(NULL, atoi(argv[1]));
	if (!target.IsValid()) {
		fprintf(stderr, "no such team\n");
		return 1;
	}
	int index = -1;
	for (int i = 0; i < 32 && index < 0; i++) {
		BMessage get(B_GET_PROPERTY), reply;
		get.AddSpecifier("Title");
		get.AddSpecifier("Window", (int32)i);
		const char* title;
		if (target.SendMessage(&get, &reply, 2000000, 2000000) != B_OK
			|| reply.FindString("result", &title) != B_OK)
			break;
		if (strstr(title, argv[2]) != NULL)
			index = i;
	}
	if (index < 0) {
		fprintf(stderr, "no window matching \"%s\"\n", argv[2]);
		return 1;
	}
	BMessage getMessenger(B_GET_PROPERTY), messengerReply;
	getMessenger.AddSpecifier("Messenger");
	getMessenger.AddSpecifier("Window", (int32)index);
	BMessenger window;
	if (target.SendMessage(&getMessenger, &messengerReply, 2000000, 2000000) != B_OK
		|| messengerReply.FindMessenger("result", &window) != B_OK) {
		fprintf(stderr, "could not get the window's messenger\n");
		return 1;
	}
	BMessenger::Private(window).SetTo(BMessenger::Private(window).Team(),
		BMessenger::Private(window).Port(), B_PREFERRED_TOKEN);

	for (int i = 3; i < argc; i++) {
		const char* step = argv[i];
		if (step[0] != '@') {
			for (const char* c = step; *c; c++)
				post(window, 0, 0, *c, *c);
			continue;
		}
		step++;
		if (strncmp(step, "sleep=", 6) == 0) {
			snooze(atoi(step + 6) * 1000LL);
			continue;
		}
		uint32 modifiers = 0;
		for (;;) {
			if (strncmp(step, "ctrl+", 5) == 0) {
				modifiers |= B_CONTROL_KEY | B_LEFT_CONTROL_KEY;
				step += 5;
			} else if (strncmp(step, "alt+", 4) == 0) {
				modifiers |= B_COMMAND_KEY | B_LEFT_COMMAND_KEY;
				step += 4;
			} else if (strncmp(step, "opt+", 4) == 0) {
				modifiers |= B_OPTION_KEY | B_LEFT_OPTION_KEY;
				step += 4;
			} else if (strncmp(step, "shift+", 6) == 0) {
				modifiers |= B_SHIFT_KEY | B_LEFT_SHIFT_KEY;
				step += 6;
			} else
				break;
		}
		bool found = false;
		for (const Named& named : kNamed) {
			if (strcmp(step, named.name) == 0) {
				post(window, named.key, modifiers, named.byte, named.byte);
				found = true;
				break;
			}
		}
		if (!found && strlen(step) == 1) {
			char c = step[0];
			char byte = (modifiers & B_CONTROL_KEY) != 0 && c >= 'a' && c <= 'z'
				? c - 'a' + 1 : c;
			post(window, 0, modifiers, c, byte);
			found = true;
		}
		if (!found) {
			fprintf(stderr, "unknown step @%s\n", step);
			return 1;
		}
	}
	return 0;
}
