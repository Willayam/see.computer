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
    const char *symbol;
} SeeMenuRow;

enum { SeeRowItem = 0, SeeRowSection = 1, SeeRowHint = 2, SeeRowSeparator = 3 };

typedef void (*SeeMenuPick)(const char *);

static const CGFloat kPanelWidth = 268.0;
static const CGFloat kItemHeight = 28.0;
static const CGFloat kSectionHeight = 34.0;
static const CGFloat kHintHeight = 22.0;
static const CGFloat kSeparatorHeight = 11.0;
static const CGFloat kPadding = 8.0;
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
@property(nonatomic, assign) BOOL highlighted;
@end

@implementation SeeMenuRowView {
    NSTrackingArea *_tracking;
}

- (void)updateTrackingAreas {
    [super updateTrackingAreas];
    if (_tracking) [self removeTrackingArea:_tracking];
    NSPoint mouse = [self convertPoint:[self.window convertPointFromScreen:NSEvent.mouseLocation]
                              fromView:nil];
    BOOL inside = self.window != nil && NSPointInRect(mouse, self.bounds);
    NSTrackingAreaOptions options = NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways;
    if (inside) options |= NSTrackingAssumeInside;
    _tracking = [[NSTrackingArea alloc]
        initWithRect:self.bounds
             options:options
               owner:self
            userInfo:nil];
    [self addTrackingArea:_tracking];
    self.highlighted = inside && self.rowId != nil;
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

// The app is never active and the panel never becomes key, so every click on a
// row is a first-mouse click, which NSView drops by default. Claiming it here,
// and claiming the mouseDown that would otherwise walk up the responder chain,
// is what makes AppKit deliver the matching mouseUp back to this view.
- (BOOL)acceptsFirstMouse:(NSEvent *__unused)event {
    return YES;
}

- (void)mouseDown:(NSEvent *__unused)event {
}

- (void)mouseUp:(NSEvent *__unused)event {
    if (!self.rowId) return;
    extern void see_menu_pick(NSString *identifier);
    see_menu_pick(self.rowId);
}
@end

static SeeMenuPanel *gPanel = nil;
static NSView *gShell = nil;
static NSView *gBody = nil;
static SeeMenuPick gPick = NULL;
static id gGlobalMonitor = nil;
static id gLocalMonitor = nil;

// The pick arrives inside -[SeeMenuRowView mouseUp:]. Redrawing there would
// free the view AppKit is still dispatching into, so the handler runs on the
// next turn of the loop instead.
void see_menu_pick(NSString *identifier) {
    if (!gPick) return;
    NSString *held = [identifier copy];
    dispatch_async(dispatch_get_main_queue(), ^{ gPick(held.UTF8String); });
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
        case SeeRowSection: return kSectionHeight;
        case SeeRowHint: return kHintHeight;
        default: return kItemHeight;
    }
}

static CGFloat fill_body(const SeeMenuRow *rows, int count) {
    CGFloat height = kPadding * 2;
    for (int i = 0; i < count; i++) height += row_height(&rows[i]);

    gBody.frame = NSMakeRect(0, 0, kPanelWidth, height);
    [gBody.subviews makeObjectsPerformSelector:@selector(removeFromSuperview)];
    CGFloat y = height - kPadding;
    for (int i = 0; i < count; i++) {
        const SeeMenuRow *row = &rows[i];
        CGFloat h = row_height(row);
        y -= h;
        NSRect frame = NSMakeRect(kPadding, y, kPanelWidth - kPadding * 2, h);

        if (row->kind == SeeRowSeparator) {
            NSBox *line = [[NSBox alloc]
                initWithFrame:NSMakeRect(kPadding + 6, y + (h - 1) / 2,
                                         kPanelWidth - kPadding * 2 - 12, 1)];
            line.boxType = NSBoxSeparator;
            [gBody addSubview:line];
            continue;
        }

        SeeMenuRowView *view = [[SeeMenuRowView alloc] initWithFrame:frame];
        view.rowId = row->id ? @(row->id) : nil;

        NSString *text = @(row->label);
        NSTextField *field;
        // One label column for every row, checked or not, so the left edge of
        // the panel reads as a single line and the checkmark hangs beside it.
        const CGFloat textLeft = 26;
        if (row->kind == SeeRowSection) {
            field = label_field(text,
                                [NSFont systemFontOfSize:11 weight:NSFontWeightSemibold],
                                NSColor.tertiaryLabelColor);
        } else if (row->kind == SeeRowHint) {
            field = label_field(text,
                                [NSFont systemFontOfSize:11 weight:NSFontWeightRegular],
                                NSColor.tertiaryLabelColor);
        } else {
            field = label_field(text, [NSFont systemFontOfSize:13], NSColor.labelColor);
        }
        CGFloat labelY = row->kind == SeeRowSection ? 0 : (h - 17) / 2;
        field.frame = NSMakeRect(textLeft, labelY, NSWidth(frame) - textLeft - 8, 17);
        view.label = field;
        view.restingColor = field.textColor;
        [view addSubview:field];

        if (row->symbol) {
            NSImage *mark = [NSImage imageWithSystemSymbolName:@(row->symbol)
                                     accessibilityDescription:nil];
            NSImageView *check = [NSImageView imageViewWithImage:mark];
            check.contentTintColor = NSColor.labelColor;
            check.frame = NSMakeRect(8, (h - 13) / 2, 13, 13);
            view.check = check;
            [view addSubview:check];
        }
        [gBody addSubview:view];
    }
    return height;
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
    gHiddenAt = NSDate.timeIntervalSinceReferenceDate;
    [gPanel orderOut:nil];
}

/// Whether the pointer is over the panel. Which window a click is delivered to
/// depends on routing the panel cannot see, and a redraw under the cursor is
/// enough to get that wrong. The rectangle is not in doubt.
static BOOL pointer_over_panel(void) {
    return gPanel != nil && gPanel.isVisible &&
           NSPointInRect(NSEvent.mouseLocation, gPanel.frame);
}

static void start_monitors(void) {
    stop_monitors();
    NSEventMask mask = NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown;
    gGlobalMonitor = [NSEvent addGlobalMonitorForEventsMatchingMask:mask
                                                            handler:^(NSEvent *__unused event) {
                                                              if (!pointer_over_panel()) see_menu_hide();
                                                            }];
    gLocalMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:mask
                                                          handler:^NSEvent *(NSEvent *event) {
                                                            if (event.window != gPanel && !pointer_over_panel()) {
                                                              see_menu_hide();
                                                            }
                                                            return event;
                                                          }];
}

int see_menu_is_open(void) {
    return gPanel != nil && gPanel.isVisible ? 1 : 0;
}

void see_menu_set_callback(SeeMenuPick pick) {
    gPick = pick;
}

static void ensure_panel(void) {
    if (gPanel) return;

    NSRect frame = NSMakeRect(0, 0, kPanelWidth, 1);
    gPanel = [[SeeMenuPanel alloc]
        initWithContentRect:frame
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

    gBody = [[NSView alloc] initWithFrame:frame];
    if (@available(macOS 26.0, *)) {
        NSGlassEffectView *glass = [[NSGlassEffectView alloc] initWithFrame:frame];
        glass.cornerRadius = kCornerRadius;
        glass.style = NSGlassEffectViewStyleRegular;
        if (@available(macOS 27.0, *)) {
            glass.effectIsInteractive = YES;
        }
        glass.contentView = gBody;
        gShell = glass;
    } else {
        NSVisualEffectView *blur = [[NSVisualEffectView alloc] initWithFrame:frame];
        blur.material = NSVisualEffectMaterialMenu;
        blur.blendingMode = NSVisualEffectBlendingModeBehindWindow;
        blur.state = NSVisualEffectStateActive;
        blur.wantsLayer = YES;
        blur.layer.cornerRadius = kCornerRadius;
        blur.layer.masksToBounds = YES;
        [blur addSubview:gBody];
        gShell = blur;
    }
    gPanel.contentView = gShell;
}

void see_menu_update(const SeeMenuRow *rows, int count) {
    if (!gPanel || !gPanel.isVisible) return;
    NSRect frame = gPanel.frame;
    CGFloat height = fill_body(rows, count);
    [gPanel setContentSize:NSMakeSize(kPanelWidth, height)];
    [gPanel setFrameOrigin:NSMakePoint(NSMinX(frame), NSMaxY(frame) - height)];
}

void see_menu_show(const SeeMenuRow *rows, int count) {
    ensure_panel();
    CGFloat height = fill_body(rows, count);
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
