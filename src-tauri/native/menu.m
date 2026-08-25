// The glass panel behind the menu-bar icon, driven from menu.rs.
//
// A non-activating NSPanel rather than an NSMenu. An NSMenu runs a modal
// tracking loop on the main thread and cannot be given a material; this panel
// takes mouse events without ever becoming key, so the app the user is
// dictating into keeps its focus and the synthetic Command+V still lands there.
#import <AppKit/AppKit.h>

typedef struct {
    const char *id;
    const char *label;
    int kind;
    int checked;
} SeeMenuRow;

enum { SeeRowItem = 0, SeeRowStatus = 1, SeeRowCaption = 2, SeeRowSeparator = 3 };

typedef void (*SeeMenuPick)(const char *);

static const CGFloat kPanelWidth = 268.0;
static const CGFloat kItemHeight = 28.0;
static const CGFloat kCaptionHeight = 22.0;
static const CGFloat kSeparatorHeight = 11.0;
static const CGFloat kPadding = 10.0;
static const CGFloat kCornerRadius = 18.0;
static const CGFloat kMenuBarGap = 6.0;

@interface SeeMenuPanel : NSPanel
@end

@implementation SeeMenuPanel
- (BOOL)canBecomeKeyWindow {
    return NO;
}
- (BOOL)canBecomeMainWindow {
    return NO;
}
@end

@interface SeeMenuRowView : NSView
@property(copy) NSString *rowId;
@property(strong) NSTextField *label;
@property(strong) NSImageView *check;
@property(strong) NSColor *restingColor;
@property(assign) BOOL highlighted;
@end

@implementation SeeMenuRowView {
    NSTrackingArea *_tracking;
}

- (void)updateTrackingAreas {
    [super updateTrackingAreas];
    if (_tracking) [self removeTrackingArea:_tracking];
    _tracking = [[NSTrackingArea alloc]
        initWithRect:self.bounds
             options:NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways
               owner:self
            userInfo:nil];
    [self addTrackingArea:_tracking];
}

- (void)setHighlighted:(BOOL)highlighted {
    _highlighted = highlighted;
    NSColor *color = highlighted ? NSColor.selectedMenuItemTextColor : self.restingColor;
    self.label.textColor = color;
    self.check.contentTintColor = color;
    self.needsDisplay = YES;
}

- (void)mouseEntered:(NSEvent *__unused)event {
    if (!self.rowId) return;
    self.highlighted = YES;
}

- (void)mouseExited:(NSEvent *__unused)event {
    self.highlighted = NO;
}

- (void)drawRect:(NSRect __unused)dirty {
    if (!self.highlighted) return;
    NSBezierPath *fill = [NSBezierPath bezierPathWithRoundedRect:NSInsetRect(self.bounds, 0, 1)
                                                        xRadius:7
                                                        yRadius:7];
    [[NSColor.controlAccentColor colorWithAlphaComponent:0.85] setFill];
    [fill fill];
}

- (void)mouseUp:(NSEvent *__unused)event {
    if (!self.rowId) return;
    extern void see_menu_pick(NSString *identifier);
    see_menu_pick(self.rowId);
}
@end

static SeeMenuPanel *gPanel = nil;
static SeeMenuPick gPick = NULL;
static id gGlobalMonitor = nil;
static id gLocalMonitor = nil;
static NSTextField *gStatusField = nil;

void see_menu_pick(NSString *identifier) {
    if (gPick) gPick(identifier.UTF8String);
}

static NSTextField *label_field(NSString *text, NSFont *font, NSColor *color) {
    NSTextField *field = [NSTextField labelWithString:text];
    field.font = font;
    field.textColor = color;
    field.lineBreakMode = NSLineBreakByTruncatingTail;
    return field;
}

static CGFloat row_height(const SeeMenuRow *row) {
    switch (row->kind) {
        case SeeRowSeparator: return kSeparatorHeight;
        case SeeRowCaption: return kCaptionHeight;
        case SeeRowStatus: return kItemHeight;
        default: return kItemHeight;
    }
}

static NSView *build_content(const SeeMenuRow *rows, int count, CGFloat *out_height) {
    CGFloat height = kPadding * 2;
    for (int i = 0; i < count; i++) height += row_height(&rows[i]);
    *out_height = height;

    NSView *content = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, kPanelWidth, height)];
    gStatusField = nil;
    CGFloat y = height - kPadding;
    for (int i = 0; i < count; i++) {
        const SeeMenuRow *row = &rows[i];
        CGFloat h = row_height(row);
        y -= h;
        NSRect frame = NSMakeRect(kPadding, y, kPanelWidth - kPadding * 2, h);

        if (row->kind == SeeRowSeparator) {
            NSView *line = [[NSView alloc]
                initWithFrame:NSMakeRect(kPadding + 6, y + h / 2, kPanelWidth - kPadding * 2 - 12, 1)];
            line.wantsLayer = YES;
            line.layer.backgroundColor = [NSColor.separatorColor colorWithAlphaComponent:0.55].CGColor;
            [content addSubview:line];
            continue;
        }

        SeeMenuRowView *view = [[SeeMenuRowView alloc] initWithFrame:frame];
        view.rowId = row->id ? @(row->id) : nil;

        NSString *text = @(row->label);
        NSTextField *field;
        // One label column for every row, checked or not, so the left edge of
        // the panel reads as a single line and the checkmark hangs beside it.
        const CGFloat textLeft = 26;
        if (row->kind == SeeRowCaption) {
            field = label_field(text,
                                [NSFont systemFontOfSize:11 weight:NSFontWeightSemibold],
                                NSColor.tertiaryLabelColor);
        } else if (row->kind == SeeRowStatus) {
            field = label_field(text,
                                [NSFont systemFontOfSize:13 weight:NSFontWeightMedium],
                                NSColor.secondaryLabelColor);
            gStatusField = field;
        } else {
            field = label_field(text, [NSFont systemFontOfSize:13], NSColor.labelColor);
        }
        field.frame = NSMakeRect(textLeft, (h - 17) / 2, NSWidth(frame) - textLeft - 8, 17);
        view.label = field;
        view.restingColor = field.textColor;
        [view addSubview:field];

        if (row->checked == 1) {
            NSImage *mark = [NSImage imageWithSystemSymbolName:@"checkmark"
                                     accessibilityDescription:nil];
            NSImageView *check = [NSImageView imageViewWithImage:mark];
            check.contentTintColor = NSColor.labelColor;
            check.frame = NSMakeRect(8, (h - 13) / 2, 13, 13);
            view.check = check;
            [view addSubview:check];
        }
        [content addSubview:view];
    }
    return content;
}

static void stop_monitors(void) {
    if (gGlobalMonitor) {
        [NSEvent removeMonitor:gGlobalMonitor];
        gGlobalMonitor = nil;
    }
    if (gLocalMonitor) {
        [NSEvent removeMonitor:gLocalMonitor];
        gLocalMonitor = nil;
    }
}

static NSTimeInterval gHiddenAt = 0;

void see_menu_hide(void) {
    stop_monitors();
    gStatusField = nil;
    gHiddenAt = NSDate.timeIntervalSinceReferenceDate;
    [gPanel orderOut:nil];
}

static void start_monitors(void) {
    stop_monitors();
    NSEventMask mask = NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown;
    gGlobalMonitor = [NSEvent addGlobalMonitorForEventsMatchingMask:mask
                                                            handler:^(NSEvent *__unused event) {
                                                              see_menu_hide();
                                                            }];
    gLocalMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:mask
                                                          handler:^NSEvent *(NSEvent *event) {
                                                            if (event.window != gPanel) see_menu_hide();
                                                            return event;
                                                          }];
}

int see_menu_is_open(void) {
    return gPanel != nil && gPanel.isVisible ? 1 : 0;
}

void see_menu_set_callback(SeeMenuPick pick) {
    gPick = pick;
}

void see_menu_set_status(const char *text) {
    if (gStatusField && text) gStatusField.stringValue = @(text);
}

void see_menu_show(const SeeMenuRow *rows, int count) {
    CGFloat height = 0;
    NSView *content = build_content(rows, count, &height);

    if (!gPanel) {
        gPanel = [[SeeMenuPanel alloc]
            initWithContentRect:NSMakeRect(0, 0, kPanelWidth, height)
                      styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
                        backing:NSBackingStoreBuffered
                          defer:NO];
        gPanel.floatingPanel = YES;
        gPanel.becomesKeyOnlyIfNeeded = YES;
        gPanel.hidesOnDeactivate = NO;
        gPanel.opaque = NO;
        gPanel.backgroundColor = NSColor.clearColor;
        gPanel.hasShadow = YES;
        gPanel.level = NSPopUpMenuWindowLevel;
        gPanel.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
                                    NSWindowCollectionBehaviorFullScreenAuxiliary |
                                    NSWindowCollectionBehaviorIgnoresCycle;
        gPanel.animationBehavior = NSWindowAnimationBehaviorUtilityWindow;
    }

    NSRect frame = NSMakeRect(0, 0, kPanelWidth, height);
    NSView *shell;
    if (@available(macOS 26.0, *)) {
        NSGlassEffectView *glass = [[NSGlassEffectView alloc] initWithFrame:frame];
        glass.cornerRadius = kCornerRadius;
        glass.style = NSGlassEffectViewStyleRegular;
        if (@available(macOS 27.0, *)) {
            glass.effectIsInteractive = YES;
        }
        glass.contentView = content;
        shell = glass;
    } else {
        NSVisualEffectView *blur = [[NSVisualEffectView alloc] initWithFrame:frame];
        blur.material = NSVisualEffectMaterialMenu;
        blur.blendingMode = NSVisualEffectBlendingModeBehindWindow;
        blur.state = NSVisualEffectStateActive;
        blur.wantsLayer = YES;
        blur.layer.cornerRadius = kCornerRadius;
        blur.layer.masksToBounds = YES;
        content.frame = blur.bounds;
        [blur addSubview:content];
        shell = blur;
    }
    gPanel.contentView = shell;
    [gPanel setContentSize:NSMakeSize(kPanelWidth, height)];

    NSPoint mouse = NSEvent.mouseLocation;
    NSScreen *screen = NSScreen.mainScreen;
    for (NSScreen *candidate in NSScreen.screens) {
        if (NSPointInRect(mouse, candidate.frame)) {
            screen = candidate;
            break;
        }
    }
    NSRect visible = screen.visibleFrame;
    CGFloat x = mouse.x - kPanelWidth / 2;
    x = MAX(NSMinX(visible) + 8, MIN(x, NSMaxX(visible) - kPanelWidth - 8));
    CGFloat top = NSMaxY(visible) - kMenuBarGap;
    [gPanel setFrameOrigin:NSMakePoint(x, top - height)];

    [gPanel orderFrontRegardless];
    start_monitors();
}

// Clicking the menu-bar icon while the panel is open reaches the dismissal
// monitor first, so by the time the icon's own handler runs the panel is
// already gone. The window after a dismissal is where that second signal
// lands, and swallowing it is what makes the icon read as a toggle.
void see_menu_toggle(const SeeMenuRow *rows, int count) {
    if (see_menu_is_open()) {
        see_menu_hide();
        return;
    }
    if (NSDate.timeIntervalSinceReferenceDate - gHiddenAt < 0.2) return;
    see_menu_show(rows, count);
}
