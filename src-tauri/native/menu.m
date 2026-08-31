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

enum {
    SeeRowItem = 0,
    SeeRowHint = 2,
    SeeRowSeparator = 3,
    SeeRowSubmenu = 4,
    SeeRowDisclosure = 5
};

typedef void (*SeeMenuPick)(const char *);
void see_menu_pick(NSString *identifier);

static const CGFloat kPanelWidth = 268.0;
static const CGFloat kSubmenuWidth = 210.0;
static const CGFloat kItemHeight = 28.0;
static const CGFloat kHintHeight = 22.0;
static const CGFloat kSeparatorHeight = 11.0;
static const CGFloat kPadding = 8.0;
// One label column for every row, checked or not, so the left edge of the
// panel reads as a single line and the glyph hangs beside it.
static const CGFloat kTextLeft = 26.0;
static const CGFloat kTextRight = 8.0;
static const CGFloat kSingleLine = 17.0;
// A dictation can be a paragraph. Three lines is where a row stops being a
// row, so the rest is an ellipsis.
static const NSInteger kMaxItemLines = 3;
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
@property(strong) NSImageView *disclosure;
@property(strong) NSColor *restingColor;
@property(nonatomic, assign) BOOL highlighted;
@property(nonatomic, assign) BOOL opensSubmenu;
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
    self.disclosure.contentTintColor = color;
    self.needsDisplay = YES;
}

- (void)mouseEntered:(NSEvent *__unused)event {
    if (!self.rowId) return;
    self.highlighted = YES;
    if (self.opensSubmenu) see_menu_pick(self.rowId);
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
static SeeMenuPanel *gSubmenuPanel = nil;
static NSView *gSubmenuShell = nil;
static NSView *gSubmenuBody = nil;
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

static NSTextField *label_field(NSString *text, NSFont *font, NSColor *color,
                                NSInteger maxLines) {
    NSTextField *field = [NSTextField labelWithString:text];
    field.font = font;
    field.textColor = color;
    field.maximumNumberOfLines = maxLines;
    if (maxLines > 1) {
        // Truncating tail is a single-line mode: a cell in it never wraps, no
        // matter how many lines it is allowed. Wrapping plus
        // `truncatesLastVisibleLine` is the pair that fills the lines and then
        // ends the last one in an ellipsis.
        field.lineBreakMode = NSLineBreakByWordWrapping;
        field.cell.wraps = YES;
        field.usesSingleLineMode = NO;
        field.cell.truncatesLastVisibleLine = YES;
    } else {
        field.lineBreakMode = NSLineBreakByTruncatingTail;
    }
    return field;
}

static NSFont *item_font(void) {
    return [NSFont systemFontOfSize:13];
}

static CGFloat item_line_height(void) {
    NSFont *font = item_font();
    return ceil(font.ascender - font.descender + font.leading);
}

/// How tall an item's label wraps, up to [`kMaxItemLines`] lines. Measured 4pt
/// narrower than the field it lands in, because the cell insets its text and a
/// line counted short would be the one the frame clips.
static CGFloat item_text_height(NSString *text, CGFloat width) {
    CGFloat line = item_line_height();
    CGFloat textWidth = width - kPadding * 2 - kTextLeft - kTextRight - 4;
    NSRect box = [text boundingRectWithSize:NSMakeSize(textWidth, CGFLOAT_MAX)
                                    options:NSStringDrawingUsesLineFragmentOrigin
                                 attributes:@{NSFontAttributeName : item_font()}];
    CGFloat lines = MAX(1.0, MIN(round(NSHeight(box) / line), (CGFloat)kMaxItemLines));
    return lines * line;
}

static CGFloat row_height(const SeeMenuRow *row, CGFloat width) {
    switch (row->kind) {
        case SeeRowSeparator: return kSeparatorHeight;
        case SeeRowHint: return kHintHeight;
        // A one-line item keeps the height it always had; a wrapped one grows
        // by what it wraps, and the panel is the sum either way.
        default:
            return MAX(kItemHeight,
                       item_text_height(@(row->label), width) + kItemHeight - kSingleLine);
    }
}

static CGFloat fill_body(NSView *body, CGFloat width, const SeeMenuRow *rows, int count) {
    CGFloat height = kPadding * 2;
    for (int i = 0; i < count; i++) height += row_height(&rows[i], width);

    body.frame = NSMakeRect(0, 0, width, height);
    [body.subviews makeObjectsPerformSelector:@selector(removeFromSuperview)];
    CGFloat y = height - kPadding;
    for (int i = 0; i < count; i++) {
        const SeeMenuRow *row = &rows[i];
        CGFloat h = row_height(row, width);
        y -= h;
        NSRect frame = NSMakeRect(kPadding, y, width - kPadding * 2, h);

        if (row->kind == SeeRowSeparator) {
            NSBox *line = [[NSBox alloc]
                initWithFrame:NSMakeRect(kPadding + 6, y + (h - 1) / 2,
                                         width - kPadding * 2 - 12, 1)];
            line.boxType = NSBoxSeparator;
            [body addSubview:line];
            continue;
        }

        SeeMenuRowView *view = [[SeeMenuRowView alloc] initWithFrame:frame];
        view.rowId = row->id ? @(row->id) : nil;
        view.opensSubmenu = row->kind == SeeRowSubmenu;

        NSString *text = @(row->label);
        NSTextField *field;
        BOOL wraps = row->kind == SeeRowItem;
        if (row->kind == SeeRowHint) {
            field = label_field(text,
                                [NSFont systemFontOfSize:11 weight:NSFontWeightRegular],
                                NSColor.tertiaryLabelColor, 1);
        } else {
            field = label_field(text, item_font(), NSColor.labelColor, kMaxItemLines);
        }
        CGFloat lineHeight = wraps ? item_line_height() : kSingleLine;
        CGFloat textHeight = wraps ? item_text_height(text, width) : kSingleLine;
        CGFloat labelY = (h - textHeight) / 2;
        field.frame =
            NSMakeRect(kTextLeft, labelY, NSWidth(frame) - kTextLeft - kTextRight, textHeight);
        view.label = field;
        view.restingColor = field.textColor;
        [view addSubview:field];

        if (row->symbol && row->kind != SeeRowDisclosure) {
            NSImage *mark = [NSImage imageWithSystemSymbolName:@(row->symbol)
                                     accessibilityDescription:nil];
            NSImageView *check = [NSImageView imageViewWithImage:mark];
            check.contentTintColor = NSColor.labelColor;
            // Beside the first line, not the middle of the block: three lines
            // of text with a glyph floating at their centre reads as unmoored.
            // On a one-line row this is the old centring exactly.
            check.frame =
                NSMakeRect(8, NSMaxY(field.frame) - lineHeight / 2 - 6.5, 13, 13);
            view.check = check;
            [view addSubview:check];
        }
        if (row->kind == SeeRowSubmenu || row->kind == SeeRowDisclosure) {
            NSString *arrowName = row->kind == SeeRowDisclosure && row->symbol
                                      ? @(row->symbol)
                                      : @"chevron.right";
            NSImage *arrow = [NSImage imageWithSystemSymbolName:arrowName
                                      accessibilityDescription:@"Submenu"];
            NSImageView *disclosure = [NSImageView imageViewWithImage:arrow];
            disclosure.contentTintColor = NSColor.secondaryLabelColor;
            disclosure.symbolConfiguration =
                [NSImageSymbolConfiguration configurationWithPointSize:10
                                                                 weight:NSFontWeightSemibold];
            disclosure.frame = NSMakeRect(NSWidth(frame) - 17, (h - 12) / 2, 12, 12);
            view.disclosure = disclosure;
            field.frame = NSMakeRect(kTextLeft, labelY,
                                     NSWidth(frame) - kTextLeft - kTextRight - 16, textHeight);
            [view addSubview:disclosure];
        }
        [body addSubview:view];
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
    [gSubmenuPanel orderOut:nil];
    [gPanel orderOut:nil];
}

/// Whether the pointer is over the panel. Which window a click is delivered to
/// depends on routing the panel cannot see, and a redraw under the cursor is
/// enough to get that wrong. The rectangle is not in doubt.
static BOOL pointer_over_panel(void) {
    BOOL overMain = gPanel != nil && gPanel.isVisible &&
                    NSPointInRect(NSEvent.mouseLocation, gPanel.frame);
    BOOL overSubmenu = gSubmenuPanel != nil && gSubmenuPanel.isVisible &&
                       NSPointInRect(NSEvent.mouseLocation, gSubmenuPanel.frame);
    return overMain || overSubmenu;
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

static void ensure_submenu_panel(void) {
    if (gSubmenuPanel) return;

    NSRect frame = NSMakeRect(0, 0, kSubmenuWidth, 1);
    gSubmenuPanel = [[SeeMenuPanel alloc]
        initWithContentRect:frame
                  styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
                    backing:NSBackingStoreBuffered
                      defer:NO];
    gSubmenuPanel.floatingPanel = YES;
    gSubmenuPanel.becomesKeyOnlyIfNeeded = YES;
    gSubmenuPanel.hidesOnDeactivate = NO;
    gSubmenuPanel.opaque = NO;
    gSubmenuPanel.backgroundColor = NSColor.clearColor;
    gSubmenuPanel.hasShadow = YES;
    gSubmenuPanel.level = NSPopUpMenuWindowLevel;
    gSubmenuPanel.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
                                      NSWindowCollectionBehaviorFullScreenAuxiliary |
                                      NSWindowCollectionBehaviorIgnoresCycle;
    gSubmenuPanel.animationBehavior = NSWindowAnimationBehaviorUtilityWindow;

    gSubmenuBody = [[NSView alloc] initWithFrame:frame];
    if (@available(macOS 26.0, *)) {
        NSGlassEffectView *glass = [[NSGlassEffectView alloc] initWithFrame:frame];
        glass.cornerRadius = kCornerRadius;
        glass.style = NSGlassEffectViewStyleRegular;
        if (@available(macOS 27.0, *)) glass.effectIsInteractive = YES;
        glass.contentView = gSubmenuBody;
        gSubmenuShell = glass;
    } else {
        NSVisualEffectView *blur = [[NSVisualEffectView alloc] initWithFrame:frame];
        blur.material = NSVisualEffectMaterialMenu;
        blur.blendingMode = NSVisualEffectBlendingModeBehindWindow;
        blur.state = NSVisualEffectStateActive;
        blur.wantsLayer = YES;
        blur.layer.cornerRadius = kCornerRadius;
        blur.layer.masksToBounds = YES;
        [blur addSubview:gSubmenuBody];
        gSubmenuShell = blur;
    }
    gSubmenuPanel.contentView = gSubmenuShell;
}

void see_menu_update(const SeeMenuRow *rows, int count) {
    if (!gPanel || !gPanel.isVisible) return;
    [gSubmenuPanel orderOut:nil];
    NSRect frame = gPanel.frame;
    CGFloat height = fill_body(gBody, kPanelWidth, rows, count);
    [gPanel setContentSize:NSMakeSize(kPanelWidth, height)];
    [gPanel setFrameOrigin:NSMakePoint(NSMinX(frame), NSMaxY(frame) - height)];
}

void see_menu_show_submenu(const SeeMenuRow *rows, int count, const char *sourceId) {
    if (!gPanel || !gPanel.isVisible || !sourceId) return;
    ensure_submenu_panel();
    CGFloat height = fill_body(gSubmenuBody, kSubmenuWidth, rows, count);
    [gSubmenuPanel setContentSize:NSMakeSize(kSubmenuWidth, height)];

    SeeMenuRowView *source = nil;
    NSString *wanted = @(sourceId);
    for (NSView *candidate in gBody.subviews) {
        if ([candidate isKindOfClass:SeeMenuRowView.class] &&
            [((SeeMenuRowView *)candidate).rowId isEqualToString:wanted]) {
            source = (SeeMenuRowView *)candidate;
            break;
        }
    }
    NSRect anchor = source ? [source convertRect:source.bounds toView:nil] : gPanel.contentView.bounds;
    anchor = [gPanel convertRectToScreen:anchor];
    NSScreen *screen = gPanel.screen ?: NSScreen.mainScreen;
    NSRect visible = screen.visibleFrame;
    CGFloat x = NSMaxX(gPanel.frame) + 4;
    if (x + kSubmenuWidth > NSMaxX(visible) - 8) x = NSMinX(gPanel.frame) - kSubmenuWidth - 4;
    CGFloat y = NSMaxY(anchor) - height + kPadding;
    y = MAX(NSMinY(visible) + 8, MIN(y, NSMaxY(visible) - height - 8));
    [gSubmenuPanel setFrameOrigin:NSMakePoint(x, y)];
    [gSubmenuPanel orderFrontRegardless];
}

void see_menu_update_submenu(const SeeMenuRow *rows, int count) {
    if (!gSubmenuPanel || !gSubmenuPanel.isVisible) return;
    NSRect frame = gSubmenuPanel.frame;
    CGFloat height = fill_body(gSubmenuBody, kSubmenuWidth, rows, count);
    [gSubmenuPanel setContentSize:NSMakeSize(kSubmenuWidth, height)];
    [gSubmenuPanel setFrameOrigin:NSMakePoint(NSMinX(frame), NSMaxY(frame) - height)];
}

void see_menu_show(const SeeMenuRow *rows, int count) {
    ensure_panel();
    CGFloat height = fill_body(gBody, kPanelWidth, rows, count);
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
