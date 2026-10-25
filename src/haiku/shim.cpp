/*
 * Temino's Haiku layer: the Be API pieces Rust can't reach directly
 * (BApplication, the window and its view, the frame bitmaps, the clipboard)
 * plus the pseudo-terminal spawn, all behind a small C ABI.
 *
 * Frames are rendered by Vulkan (Venus) into host memory, so presenting is a
 * copy into a BBitmap followed by DrawBitmap() on the window thread. Two
 * bitmaps alternate: the core thread fills the back one while the window
 * thread may still be drawing the front one.
 */

#include <Alert.h>
#include <Application.h>
#include <Autolock.h>
#include <Beep.h>
#include <Bitmap.h>
#include <Clipboard.h>
#include <Cursor.h>
#include <Locker.h>
#include <MenuBar.h>
#include <MenuItem.h>
#include <Message.h>
#include <Screen.h>
#include <String.h>
#include <View.h>
#include <Window.h>
#include <image.h>

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <pthread.h>
#include <pwd.h>
#include <unistd.h>

#include <atomic>


extern "C" {

struct TeminoEvent {
	int32_t kind;
	int32_t key;
	uint32_t modifiers;
	int32_t buttons;
	int32_t clicks;
	float x;
	float y;
	float dx;
	float dy;
	int32_t width;
	int32_t height;
	int32_t raw_char;
	int32_t nbytes;
	uint8_t bytes[16];
};

typedef void (*TeminoEventCallback)(const TeminoEvent* event, void* context);

}


enum {
	kEventKey = 1,
	kEventMouseDown = 2,
	kEventMouseUp = 3,
	kEventMouseMoved = 4,
	kEventWheel = 5,
	kEventResize = 6,
	kEventFocus = 7,
	kEventClose = 8,
	kEventQuit = 9,
	kEventBackground = 10,
	kEventShell = 11,
};

enum {
	kMsgFrame = 'rfrm',
	kMsgTitle = 'rttl',
	kMsgCursor = 'rcur',
	kMsgMenuKey = 'rmky',
	kMsgBackground = 'rbgd',
	kMsgBackgroundMenu = 'rbgm',
	kMsgShell = 'rshl',
	kMsgShellMenu = 'rshm',
};


static TeminoEventCallback sCallback;
static void* sContext;

static BLocker sFrameLock("temino frame");
static BBitmap* sFront;
static BBitmap* sBack;
static std::atomic<bool> sFramePending(false);
static rgb_color sBackground = { 16, 18, 24, 255 };

static class TeminoWindow* sWindow;
static BMenu* sBackgroundMenu;
static BMenu* sShellMenu;
static class TeminoView* sView;
static std::atomic<bool> sQuitting(false);


static void
emit(TeminoEvent& event)
{
	if (sCallback != NULL)
		sCallback(&event, sContext);
}


static int32
current_int32(BView* view, const char* name, int32 fallback)
{
	BMessage* message = view->Window()->CurrentMessage();
	int32 value;
	if (message != NULL && message->FindInt32(name, &value) == B_OK)
		return value;
	return fallback;
}


class TeminoView : public BView {
public:
	TeminoView(BRect frame)
		:
		BView(frame, "temino", B_FOLLOW_ALL,
			B_WILL_DRAW | B_FRAME_EVENTS | B_FULL_UPDATE_ON_RESIZE),
		fIBeam(B_CURSOR_ID_I_BEAM),
		fArrow(B_CURSOR_ID_SYSTEM_DEFAULT),
		fResizeColumn(B_CURSOR_ID_RESIZE_EAST_WEST),
		fResizeRow(B_CURSOR_ID_RESIZE_NORTH_SOUTH)
	{
		SetViewColor(B_TRANSPARENT_COLOR);
		SetEventMask(0, 0);
	}

	void AttachedToWindow() override
	{
		MakeFocus(true);
		SetViewCursor(&fIBeam);
	}

	void Draw(BRect update) override
	{
		BAutolock lock(sFrameLock);
		_DrawFrame();
	}

	void FrameResized(float width, float height) override
	{
		TeminoEvent event = {};
		event.kind = kEventResize;
		event.width = (int32)width + 1;
		event.height = (int32)height + 1;
		emit(event);
	}

	void MouseDown(BPoint where) override
	{
		SetMouseEventMask(B_POINTER_EVENTS, B_NO_POINTER_HISTORY);
		TeminoEvent event = {};
		event.kind = kEventMouseDown;
		event.x = where.x;
		event.y = where.y;
		event.buttons = current_int32(this, "buttons", B_PRIMARY_MOUSE_BUTTON);
		event.clicks = current_int32(this, "clicks", 1);
		event.modifiers = modifiers();
		emit(event);
	}

	void MouseUp(BPoint where) override
	{
		TeminoEvent event = {};
		event.kind = kEventMouseUp;
		event.x = where.x;
		event.y = where.y;
		event.buttons = current_int32(this, "buttons", 0);
		event.modifiers = modifiers();
		emit(event);
	}

	void MouseMoved(BPoint where, uint32 transit, const BMessage* drag) override
	{
		TeminoEvent event = {};
		event.kind = kEventMouseMoved;
		event.x = where.x;
		event.y = where.y;
		event.buttons = current_int32(this, "buttons", 0);
		event.modifiers = modifiers();
		emit(event);
	}

	void MessageReceived(BMessage* message) override
	{
		switch (message->what) {
			case kMsgFrame:
			{
				sFramePending.store(false);
				BAutolock lock(sFrameLock);
				_DrawFrame();
				Sync();
				break;
			}
			case kMsgCursor:
			{
				int32 kind = message->GetInt32("kind", 0);
				const BCursor* cursors[] = {&fIBeam, &fArrow, &fResizeColumn, &fResizeRow};
				SetViewCursor(cursors[kind >= 0 && kind < 4 ? kind : 0]);
				break;
			}
			case B_MOUSE_WHEEL_CHANGED:
			{
				TeminoEvent event = {};
				event.kind = kEventWheel;
				event.dx = message->GetFloat("be:wheel_delta_x", 0);
				event.dy = message->GetFloat("be:wheel_delta_y", 0);
				event.modifiers = modifiers();
				BPoint where;
				uint32 buttons;
				GetMouse(&where, &buttons, false);
				event.x = where.x;
				event.y = where.y;
				emit(event);
				break;
			}
			default:
				BView::MessageReceived(message);
		}
	}

private:
	// Called with sFrameLock held and the window locked.
	void _DrawFrame()
	{
		BRect bounds = Bounds();
		if (sFront == NULL) {
			SetHighColor(sBackground);
			FillRect(bounds);
			return;
		}
		BRect frame = sFront->Bounds();
		DrawBitmap(sFront, B_ORIGIN);
		SetHighColor(sBackground);
		if (bounds.right > frame.right)
			FillRect(BRect(frame.right + 1, 0, bounds.right, bounds.bottom));
		if (bounds.bottom > frame.bottom)
			FillRect(BRect(0, frame.bottom + 1, frame.right, bounds.bottom));
	}

	BCursor fIBeam;
	BCursor fArrow;
	BCursor fResizeColumn;
	BCursor fResizeRow;
};


class TeminoWindow : public BWindow {
public:
	TeminoWindow(BRect frame, const char* title)
		:
		BWindow(frame, title, B_TITLED_WINDOW, B_ASYNCHRONOUS_CONTROLS)
	{
		SetSizeLimits(320, 32768, 200, 32768);
	}

	// Every key goes to Rust, shortcuts included: the terminal decides what
	// Alt (Haiku's Command key) combinations mean. The system's own keys
	// stay with BWindow: Ctrl+Tab (and Ctrl+`) open Deskbar's switcher,
	// PrtScr takes a screenshot.
	void DispatchMessage(BMessage* message, BHandler* target) override
	{
		if (message->what == B_KEY_DOWN && _IsSystemKey(message)) {
			BWindow::DispatchMessage(message, target);
			return;
		}
		if (message->what == B_KEY_DOWN) {
			TeminoEvent event = {};
			event.kind = kEventKey;
			event.key = message->GetInt32("key", 0);
			event.modifiers = (uint32)message->GetInt32("modifiers", 0);
			event.raw_char = message->GetInt32("raw_char", 0);
			const char* bytes;
			if (message->FindString("bytes", &bytes) == B_OK) {
				size_t length = strnlen(bytes, sizeof(event.bytes));
				memcpy(event.bytes, bytes, length);
				event.nbytes = (int32)length;
			}
			// Control+Space and Control+@ produce an empty string.
			if (event.nbytes == 0 && (event.modifiers & B_CONTROL_KEY) != 0
				&& event.raw_char == ' ') {
				event.nbytes = 1;
			}
			emit(event);
			return;
		}
		if (message->what == B_KEY_UP || message->what == B_UNMAPPED_KEY_DOWN
			|| message->what == B_UNMAPPED_KEY_UP) {
			return;
		}
		BWindow::DispatchMessage(message, target);
	}

	static bool _IsSystemKey(BMessage* message)
	{
		int32 rawKey = message->GetInt32("key", 0);
		uint32 mods = (uint32)message->GetInt32("modifiers", 0);
		const char* bytes = message->GetString("bytes", "");
		if ((bytes[0] == B_TAB || rawKey == 0x11) && (mods & B_CONTROL_KEY) != 0)
			return true;
		return bytes[0] == B_FUNCTION_KEY && rawKey == B_PRINT_KEY;
	}

	void MessageReceived(BMessage* message) override
	{
		if (message->what == kMsgTitle) {
			SetTitle(message->GetString("title", "Temino"));
			return;
		}
		if (message->what == kMsgBackground || message->what == kMsgShell) {
			TeminoEvent event = {};
			event.kind = message->what == kMsgBackground ? kEventBackground : kEventShell;
			event.key = message->GetInt32("index", 0);
			emit(event);
			return;
		}
		if (message->what == kMsgBackgroundMenu) {
			_FillChoices(sBackgroundMenu, kMsgBackground, message);
			return;
		}
		if (message->what == kMsgShellMenu) {
			_FillChoices(sShellMenu, kMsgShell, message);
			return;
		}
		if (message->what == kMsgMenuKey) {
			// A menu item acts as its own shortcut: the same key event goes
			// to Rust, so menus and keys share one code path.
			TeminoEvent event = {};
			event.kind = kEventKey;
			event.key = message->GetInt32("key", 0);
			event.modifiers = (uint32)message->GetInt32("modifiers", 0);
			event.raw_char = message->GetInt32("raw_char", 0);
			event.bytes[0] = (uint8)event.raw_char;
			event.nbytes = 1;
			emit(event);
			return;
		}
		BWindow::MessageReceived(message);
	}

	// Rust names a menu's choices and which is picked; picks come back as
	// `what` messages and the app refills the menu with the new mark.
	static void _FillChoices(BMenu* menu, uint32 what, BMessage* message)
	{
		while (BMenuItem* item = menu->RemoveItem((int32)0))
			delete item;
		int32 selected = message->GetInt32("selected", 0);
		const char* name;
		for (int32 i = 0; message->FindString("name", i, &name) == B_OK; i++) {
			BMessage* pick = new BMessage(what);
			pick->AddInt32("index", i);
			BMenuItem* item = new BMenuItem(name, pick);
			item->SetMarked(i == selected);
			menu->AddItem(item);
		}
	}

	void WindowActivated(bool active) override
	{
		TeminoEvent event = {};
		event.kind = kEventFocus;
		event.buttons = active ? 1 : 0;
		emit(event);
		BWindow::WindowActivated(active);
	}

	bool QuitRequested() override
	{
		if (sQuitting.load())
			return true;
		TeminoEvent event = {};
		event.kind = kEventClose;
		emit(event);
		return false;
	}
};


class TeminoApplication : public BApplication {
public:
	TeminoApplication(const char* signature)
		:
		BApplication(signature)
	{
	}

	bool QuitRequested() override
	{
		if (sQuitting.load())
			return true;
		TeminoEvent event = {};
		event.kind = kEventClose;
		emit(event);
		return false;
	}
};


// A menu item that sends Command+`shortcut` (plus `modifiers`) to the
// terminal. Its shortcut is drawn in the menu; BWindow never matches it
// because TeminoWindow hands every key to Rust first.
static BMenuItem*
menu_key(const char* label, char shortcut, uint32 modifiers = 0,
	int32 key = 0, char bytes = 0)
{
	BMessage* message = new BMessage(kMsgMenuKey);
	message->AddInt32("key", key);
	message->AddInt32("modifiers", (int32)(B_COMMAND_KEY | modifiers));
	message->AddInt32("raw_char", bytes != 0 ? bytes : shortcut);
	return new BMenuItem(label, message, shortcut, modifiers);
}


static BMenuBar*
build_menu_bar()
{
	BMenuBar* bar = new BMenuBar(BRect(0, 0, 0, 0), "menu");

	BMenu* app = new BMenu("Temino");
	app->AddItem(menu_key("New tab", 'T'));
	app->AddItem(menu_key("Split right", 'D'));
	app->AddItem(menu_key("Split down", 'E'));
	app->AddSeparatorItem();
	app->AddItem(menu_key("Close pane", 'W'));
	app->AddItem(menu_key("Quit", 'Q'));
	bar->AddItem(app);

	BMenu* edit = new BMenu("Edit");
	edit->AddItem(menu_key("Copy", 'C'));
	edit->AddItem(menu_key("Copy command", 'C', B_SHIFT_KEY));
	edit->AddItem(menu_key("Paste", 'V'));
	edit->AddItem(menu_key("Select all", 'A'));
	edit->AddSeparatorItem();
	edit->AddItem(menu_key("Clear", 'K'));
	bar->AddItem(edit);

	BMenu* view = new BMenu("View");
	view->AddItem(menu_key("Show/hide tabs", 'B'));
	view->AddItem(menu_key("Previous tab", '{', B_SHIFT_KEY));
	view->AddItem(menu_key("Next tab", '}', B_SHIFT_KEY));
	view->AddItem(menu_key("Previous pane", '['));
	view->AddItem(menu_key("Next pane", ']'));
	// Enter has no shortcut character, so the label carries it.
	BMessage* zoom = new BMessage(kMsgMenuKey);
	zoom->AddInt32("key", 0x47);
	zoom->AddInt32("modifiers", (int32)B_COMMAND_KEY);
	zoom->AddInt32("raw_char", B_ENTER);
	view->AddItem(new BMenuItem("Zoom pane (Alt+Enter)", zoom));
	view->AddSeparatorItem();
	view->AddItem(menu_key("Bigger text", '+'));
	view->AddItem(menu_key("Smaller text", '-'));
	view->AddItem(menu_key("Default text size", '0'));
	bar->AddItem(view);

	BMenu* settings = new BMenu("Settings");
	sBackgroundMenu = new BMenu("Background");
	settings->AddItem(sBackgroundMenu);
	sShellMenu = new BMenu("Shell");
	settings->AddItem(sShellMenu);
	bar->AddItem(settings);

	return bar;
}


// Hands a choice menu's entries to the window, to fill on its thread.
static void
post_choices(uint32 what, const char* const* names, int32 count,
	int32 selected)
{
	BMessage message(what);
	for (int32 i = 0; i < count; i++)
		message.AddString("name", names[i]);
	message.AddInt32("selected", selected);
	sWindow->PostMessage(&message);
}


extern "C" {


void
temino_app_new(const char* signature)
{
	new TeminoApplication(signature);
}


void
temino_window_new(const char* title, int32_t width, int32_t height,
	TeminoEventCallback callback, void* context)
{
	sCallback = callback;
	sContext = context;

	BRect screen = BScreen().Frame();
	float left = floorf((screen.Width() - width) / 2);
	float top = floorf((screen.Height() - height) / 2.5f);
	BRect frame(left, top, left + width - 1, top + height - 1);

	sWindow = new TeminoWindow(frame, title);
	BMenuBar* menuBar = build_menu_bar();
	sWindow->AddChild(menuBar);
	// The terminal keeps the size Rust asked for; the window grows by the
	// menu bar's height.
	float menuHeight = menuBar->Frame().Height() + 1;
	sWindow->ResizeBy(0, menuHeight);
	BRect viewFrame = sWindow->Bounds();
	viewFrame.top = menuHeight;
	sView = new TeminoView(viewFrame);
	sWindow->AddChild(sView);
	sWindow->Show();
}


void
temino_app_run()
{
	be_app->Run();
	sQuitting.store(true);
	delete be_app;
}


void
temino_quit()
{
	sQuitting.store(true);
	be_app->PostMessage(B_QUIT_REQUESTED);
}


void
temino_set_background(uint8_t r, uint8_t g, uint8_t b)
{
	BAutolock lock(sFrameLock);
	sBackground = make_color(r, g, b, 255);
}


void
temino_set_background_menu(const char* const* names, int32_t count,
	int32_t selected)
{
	post_choices(kMsgBackgroundMenu, names, count, selected);
}


void
temino_set_shell_menu(const char* const* names, int32_t count,
	int32_t selected)
{
	post_choices(kMsgShellMenu, names, count, selected);
}


void
temino_present(const uint8_t* pixels, int32_t width, int32_t height,
	int32_t stride)
{
	if (sQuitting.load() || width <= 0 || height <= 0)
		return;
	{
		BAutolock lock(sFrameLock);
		if (sBack == NULL || sBack->Bounds().IntegerWidth() + 1 != width
			|| sBack->Bounds().IntegerHeight() + 1 != height) {
			delete sBack;
			sBack = new BBitmap(BRect(0, 0, width - 1, height - 1), B_RGB32);
		}
		uint8* destination = (uint8*)sBack->Bits();
		int32 destinationStride = sBack->BytesPerRow();
		int32 rowBytes = width * 4;
		if (stride == destinationStride && stride == rowBytes)
			memcpy(destination, pixels, (size_t)rowBytes * height);
		else {
			for (int32 y = 0; y < height; y++) {
				memcpy(destination + (size_t)y * destinationStride,
					pixels + (size_t)y * stride, rowBytes);
			}
		}
		BBitmap* swap = sFront;
		sFront = sBack;
		sBack = swap;
	}
	if (!sFramePending.exchange(true))
		sWindow->PostMessage(kMsgFrame, sView);
}


void
temino_set_title(const char* title)
{
	if (sQuitting.load())
		return;
	BMessage message(kMsgTitle);
	message.AddString("title", title);
	sWindow->PostMessage(&message);
}


void
temino_set_cursor(int32_t kind)
{
	if (sQuitting.load())
		return;
	BMessage message(kMsgCursor);
	message.AddInt32("kind", kind);
	sWindow->PostMessage(&message, sView);
}


void
temino_beep()
{
	beep();
}


int32_t
temino_clipboard_set(const char* text, int32_t length)
{
	if (!be_clipboard->Lock())
		return -1;
	be_clipboard->Clear();
	BMessage* data = be_clipboard->Data();
	data->AddData("text/plain", B_MIME_TYPE, text, length);
	status_t status = be_clipboard->Commit();
	be_clipboard->Unlock();
	return status == B_OK ? 0 : -1;
}


// Returns a malloc()ed copy of the clipboard text (free it with
// temino_free), or NULL.
char*
temino_clipboard_get(int32_t* length)
{
	*length = 0;
	if (!be_clipboard->Lock())
		return NULL;
	char* copy = NULL;
	const char* text;
	ssize_t textLength;
	BMessage* data = be_clipboard->Data();
	if (data != NULL
		&& data->FindData("text/plain", B_MIME_TYPE, (const void**)&text,
			&textLength) == B_OK) {
		copy = (char*)malloc(textLength + 1);
		memcpy(copy, text, textLength);
		copy[textLength] = '\0';
		*length = (int32_t)textLength;
	}
	be_clipboard->Unlock();
	return copy;
}


void
temino_free(void* pointer)
{
	free(pointer);
}


static void
initialize_termios(struct termios& tio)
{
	// The same line discipline Haiku's Terminal sets up.
	tio.c_line = 0;
	tio.c_iflag &= ~(INLCR | IGNCR | ISTRIP | IGNBRK | BRKINT);
	tio.c_iflag |= ICRNL;
	tio.c_oflag &= ~(OCRNL | ONLRET | NLDLY | CRDLY | TABDLY | BSDLY | VTDLY
		| FFDLY);
	tio.c_oflag |= ONLCR | OPOST;
	tio.c_cflag &= ~(CBAUD | CSIZE);
	tio.c_cflag |= B19200 | CS8 | CREAD | HUPCL;
	tio.c_lflag |= ISIG | ICANON | ECHO | ECHOE | ECHONL;
	tio.c_lflag &= ~(ECHOK | IEXTEN);
	tio.c_cc[VINTR] = 'C' & 0x1f;
	tio.c_cc[VQUIT] = '\\' & 0x1f;
	tio.c_cc[VERASE] = 0x7f;
	tio.c_cc[VKILL] = 'U' & 0x1f;
	tio.c_cc[VEOF] = 'D' & 0x1f;
	tio.c_cc[VEOL] = 0;
	tio.c_cc[VMIN] = 4;
	tio.c_cc[VTIME] = 0;
	tio.c_cc[VEOL2] = 0;
	tio.c_cc[VSWTCH] = 0;
	tio.c_cc[VSTART] = 'Q' & 0x1f;
	tio.c_cc[VSTOP] = 'S' & 0x1f;
	tio.c_cc[VSUSP] = 'Z' & 0x1f;
}


// Starts a program on a new pseudo-terminal without fork(). Haiku's fork()
// makes every private area copy-on-write, including the Venus blob
// mappings this process holds once Vulkan is up, which breaks the command
// ring (Venus aborts) and later hangs the kernel when the areas go away.
// So the shell is started with load_image() as `self --pty-exec <tty>
// <columns> <rows> <cwd> <program> [args]`, and that fresh team sets up
// the terminal (temino_pty_exec) before exec'ing the program.
// Returns the team id and stores the master fd, or returns an error code
// (negative on Haiku).
int32_t
temino_spawn(const char* self, const char* const* argv, int32_t argc,
	const char* const* envp, const char* cwd, int32_t columns, int32_t rows,
	int32_t* masterOut)
{
	int master = posix_openpt(O_RDWR | O_NOCTTY);
	if (master < 0)
		return errno;
	fcntl(master, F_SETFD, FD_CLOEXEC);
	const char* ttyName;
	if (grantpt(master) != 0 || unlockpt(master) != 0
		|| (ttyName = ptsname(master)) == NULL) {
		int error = errno;
		close(master);
		return error;
	}

	char columnsText[16];
	char rowsText[16];
	snprintf(columnsText, sizeof(columnsText), "%d", (int)columns);
	snprintf(rowsText, sizeof(rowsText), "%d", (int)rows);
	const char* args[argc + 7];
	int count = 0;
	args[count++] = self;
	args[count++] = "--pty-exec";
	args[count++] = ttyName;
	args[count++] = columnsText;
	args[count++] = rowsText;
	args[count++] = cwd != NULL ? cwd : "";
	for (int32_t i = 0; i < argc; i++)
		args[count++] = argv[i];
	args[count] = NULL;

	thread_id team = load_image(count, args, (const char**)envp);
	if (team < 0) {
		close(master);
		return team;
	}
	resume_thread(team);
	*masterOut = master;
	return team;
}


// The child side of temino_spawn: argv is <tty> <columns> <rows> <cwd>
// <program> [args]. Only returns if something failed.
int32_t
temino_pty_exec(int32_t argc, const char* const* argv)
{
	if (argc < 5)
		return 2;
	const char* tty = argv[0];
	int columns = atoi(argv[1]);
	int rows = atoi(argv[2]);
	const char* cwd = argv[3];

	// A new session; the first tty it opens becomes its controlling tty.
	if (setsid() < 0)
		return 126;
	int slave = open(tty, O_RDWR);
	if (slave < 0)
		return 126;

	signal(SIGCHLD, SIG_DFL);
	signal(SIGHUP, SIG_DFL);
	signal(SIGQUIT, SIG_DFL);
	signal(SIGTERM, SIG_DFL);
	signal(SIGINT, SIG_DFL);
	signal(SIGTTOU, SIG_DFL);
	signal(SIGPIPE, SIG_DFL);
	sigset_t none;
	sigemptyset(&none);
	pthread_sigmask(SIG_SETMASK, &none, NULL);

	struct termios tio;
	tcgetattr(slave, &tio);
	initialize_termios(tio);
	dup2(slave, 0);
	dup2(slave, 1);
	dup2(slave, 2);
	if (slave > 2)
		close(slave);
	tcsetattr(0, TCSANOW, &tio);

	struct winsize size = {};
	size.ws_row = rows;
	size.ws_col = columns;
	ioctl(0, TIOCSWINSZ, &size, sizeof(size));
	tcsetpgrp(0, getpgrp());

	if (cwd[0] != '\0')
		chdir(cwd);
	execv(argv[4], (char* const*)(argv + 4));
	return 127;
}


// A modal alert, for fatal errors (no fork, unlike running `alert`).
void
temino_alert(const char* text)
{
	if (be_app == NULL)
		return;
	BAlert* alert = new BAlert("Temino", text, "Quit", NULL, NULL,
		B_WIDTH_AS_USUAL, B_STOP_ALERT);
	alert->Go();
}


int32_t
temino_pty_resize(int32_t master, int32_t columns, int32_t rows,
	int32_t pixelWidth, int32_t pixelHeight)
{
	struct winsize size = {};
	size.ws_row = rows;
	size.ws_col = columns;
	size.ws_xpixel = pixelWidth;
	size.ws_ypixel = pixelHeight;
	return ioctl(master, TIOCSWINSZ, &size, sizeof(size)) == 0 ? 0 : errno;
}


// Blocks until the child exits; returns its exit code (128 + signal when
// it was killed).
int32_t
temino_wait(int32_t pid)
{
	int status;
	while (waitpid(pid, &status, 0) < 0) {
		if (errno != EINTR)
			return -1;
	}
	if (WIFEXITED(status))
		return WEXITSTATUS(status);
	if (WIFSIGNALED(status))
		return 128 + WTERMSIG(status);
	return -1;
}


void
temino_hangup(int32_t pid)
{
	kill(-pid, SIGHUP);
	kill(pid, SIGHUP);
}


// The login name, for when $USER isn't set (launched from Deskbar).
const char*
temino_user_name()
{
	struct passwd* entry = getpwuid(getuid());
	return entry != NULL ? entry->pw_name : NULL;
}


}
