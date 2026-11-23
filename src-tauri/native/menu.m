// The glass panel behind the menu-bar icon, driven from menu.rs.
//
// A non-activating NSPanel rather than an NSMenu. An NSMenu runs a modal
// tracking loop on the main thread and cannot be given a material; this panel
// takes mouse events without ever becoming key, so the app the user is
// dictating into keeps its focus and the synthetic Command+V still lands there.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>

void see_readback_app_terminated(int pid);

void see_watch_app_termination(void) {
    [[[NSWorkspace sharedWorkspace] notificationCenter]
        addObserverForName:NSWorkspaceDidTerminateApplicationNotification
        object:nil queue:nil usingBlock:^(NSNotification *notification) {
            NSRunningApplication *app = notification.userInfo[NSWorkspaceApplicationKey];
            if (app) see_readback_app_terminated(app.processIdentifier);
        }];
}

void see_library_close_on_command_q(void) {
    [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown handler:^NSEvent *(NSEvent *event) {
        NSEventModifierFlags flags = event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask;
        NSWindow *window = NSApp.keyWindow;
        if (event.keyCode == 12 && flags == NSEventModifierFlagCommand &&
            [window.title isEqualToString:@"Library"]) {
            [window close];
            return nil;
        }
        return event;
    }];
}

typedef struct {
    const char *id;
    const char *label;
    int kind;
    const char *symbol;
} SeeMenuRow;

enum {
    SeeRowItem = 0,
    SeeRowLine = 1,
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
/// The stats header, and the speed bars inside it that wait for a wpm.
static const CGFloat kHeaderHeight = 204.0;
static const CGFloat kHeaderSpeedHeight = 74.0;
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

typedef struct {
    int present;
    uint64_t total, previous, delta;
    double wpm, times_typing, progress;
    int level_index;
    const char *since_label, *today_label, *saved_label, *level_title, *level_detail;
} SeeMenuHeader;

static SeeMenuHeader gHeader;
static NSString *gSinceLabel, *gTodayLabel, *gSavedLabel, *gLevelTitle, *gLevelDetail;
static BOOL gOpeningHeader;

void see_menu_set_header(const SeeMenuHeader *header) {
    gHeader = header ? *header : (SeeMenuHeader){0};
    gSinceLabel = header ? @(header->since_label) : @"";
    gTodayLabel = header ? @(header->today_label) : @"";
    gSavedLabel = header ? @(header->saved_label) : @"";
    gLevelTitle = header ? @(header->level_title) : @"";
    gLevelDetail = header ? @(header->level_detail) : @"";
}

static NSString *header_number(uint64_t value) {
    NSNumberFormatter *formatter = [[NSNumberFormatter alloc] init];
    formatter.numberStyle = NSNumberFormatterDecimalStyle;
    formatter.locale = [NSLocale localeWithLocaleIdentifier:@"en_US"];
    return [formatter stringFromNumber:@(value)];
}

@interface SeeMenuHeaderView : NSView
@end

@implementation SeeMenuHeaderView
- (BOOL)isFlipped { return YES; }

- (void)text:(NSString *)text x:(CGFloat)x y:(CGFloat)y width:(CGFloat)width
        size:(CGFloat)size weight:(NSFontWeight)weight color:(NSColor *)color {
    if (!text.length) return;
    NSTextField *field = label_field(text, [NSFont monospacedDigitSystemFontOfSize:size weight:weight], color, 1);
    field.frame = NSMakeRect(x, y, width, size + 5);
    [self addSubview:field];
}

- (void)viewDidChangeEffectiveAppearance {
    [super viewDidChangeEffectiveAppearance];
    if (!self.layer) return;
    [self.effectiveAppearance performAsCurrentDrawingAppearance:^{
        NSMutableArray<CALayer *> *layers = [NSMutableArray arrayWithObject:self.layer];
        while (layers.count) {
            CALayer *layer = layers.lastObject;
            [layers removeLastObject];
            if ([layer isKindOfClass:CATextLayer.class]) ((CATextLayer *)layer).foregroundColor = NSColor.labelColor.CGColor;
            NSColor *background = [layer valueForKey:@"seeBackground"];
            if (background) layer.backgroundColor = background.CGColor;
            NSColor *stroke = [layer valueForKey:@"seeStroke"];
            if (stroke) ((CAShapeLayer *)layer).strokeColor = stroke.CGColor;
            [layers addObjectsFromArray:layer.sublayers ?: @[]];
        }
    }];
}

- (void)build:(BOOL)opening {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    [self.subviews makeObjectsPerformSelector:@selector(removeFromSuperview)];
    self.wantsLayer = YES;
    self.layer.sublayers = nil;
    BOOL motion = opening && !NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion;
    NSColor *accent = [NSColor colorWithName:nil dynamicProvider:^NSColor *(NSAppearance *appearance) {
        BOOL dark = [[appearance bestMatchFromAppearancesWithNames:@[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]] isEqualToString:NSAppearanceNameDarkAqua];
        return dark ? [NSColor colorWithSRGBRed:90/255.0 green:200/255.0 blue:250/255.0 alpha:1] : [NSColor colorWithSRGBRed:26/255.0 green:166/255.0 blue:228/255.0 alpha:1];
    }];
    NSColor *accentInk = [NSColor colorWithName:nil dynamicProvider:^NSColor *(NSAppearance *appearance) {
        BOOL dark = [[appearance bestMatchFromAppearancesWithNames:@[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]] isEqualToString:NSAppearanceNameDarkAqua];
        return dark ? accent : [NSColor colorWithSRGBRed:10/255.0 green:127/255.0 blue:180/255.0 alpha:1];
    }];
    [self text:gSinceLabel
             x:8 y:7 width:160 size:11 weight:NSFontWeightRegular color:NSColor.secondaryLabelColor];
    if (gTodayLabel.length) {
        NSTextField *todayField = label_field(gTodayLabel, [NSFont monospacedDigitSystemFontOfSize:11 weight:NSFontWeightMedium], NSColor.labelColor, 1);
        todayField.alignment = NSTextAlignmentRight;
        todayField.frame = NSMakeRect(160, 7, 84, 16);
        [self addSubview:todayField];
    }

    NSString *digits = [NSString stringWithFormat:@"%llu", (unsigned long long)gHeader.total];
    NSString *gainText = gHeader.delta ? [@"+" stringByAppendingString:header_number(gHeader.delta)] : @"";
    CGFloat gainWidth = gainText.length ? [gainText sizeWithAttributes:@{NSFontAttributeName:[NSFont monospacedDigitSystemFontOfSize:14 weight:NSFontWeightSemibold]}].width + 4 : 0;
    CGFloat size = MIN(32, (236 - 51 - gainWidth - ((digits.length - 1) / 3) * 8) / (digits.length * .62));
    CGFloat h = 38, digitWidth = size * .62, x = 8;
    uint64_t place = 1;
    for (NSUInteger i = 1; i < digits.length; i++) place *= 10;
    for (NSUInteger i = 0; i < digits.length; i++) {
        if (i && (digits.length - i) % 3 == 0) {
            [self text:@"," x:x y:25 width:9 size:size weight:NSFontWeightSemibold color:NSColor.labelColor];
            x += 8;
        }
        NSView *wheel = [[NSView alloc] initWithFrame:NSMakeRect(x, 25, digitWidth, h)];
        wheel.wantsLayer = YES;
        wheel.layer.masksToBounds = YES;
        wheel.layer.geometryFlipped = YES;
        [self addSubview:wheel];
        // The extra zero makes the 9-to-0 seam identical on either side of a wrap.
        CALayer *strip = [CALayer layer];
        strip.frame = CGRectMake(0, 0, digitWidth, h * 11);
        [wheel.layer addSublayer:strip];
        NSFont *font = [NSFont monospacedDigitSystemFontOfSize:size weight:NSFontWeightSemibold];
        for (int n = 0; n <= 10; n++) {
            CATextLayer *text = [CATextLayer layer];
            text.frame = CGRectMake(0, n * h, digitWidth, h);
            text.string = [NSString stringWithFormat:@"%d", n % 10];
            text.font = (__bridge CFTypeRef)font;
            text.fontSize = size;
            [self.effectiveAppearance performAsCurrentDrawingAppearance:^{ text.foregroundColor = NSColor.labelColor.CGColor; }];
            text.contentsScale = NSScreen.mainScreen.backingScaleFactor;
            text.alignmentMode = kCAAlignmentCenter;
            [strip addSublayer:text];
        }
        double final = (gHeader.total / place) % 10;
        strip.transform = CATransform3DMakeTranslation(0, -final * h, 0);
        if (motion && gHeader.total / place != gHeader.previous / place) {
            NSMutableArray *values = [NSMutableArray array];
            for (int frame = 0; frame <= 120; frame++) {
                double t = frame / 120.0;
                double value = gHeader.previous + (gHeader.total - gHeader.previous) * (1 - pow(1 - t, 4));
                double quotient = floor(value / place);
                double carry = place == 1 ? value - floor(value) : MAX(0, fmod(value, place) - (place - 1));
                [values addObject:@(-(fmod(quotient, 10) + carry) * h)];
            }
            CAKeyframeAnimation *roll = [CAKeyframeAnimation animationWithKeyPath:@"transform.translation.y"];
            roll.values = values;
            roll.calculationMode = kCAAnimationDiscrete;
            roll.duration = 1;
            [strip addAnimation:roll forKey:@"roll"];
        }
        x += digitWidth;
        place /= 10;
    }
    [self text:@"words" x:x+4 y:39 width:52 size:14 weight:NSFontWeightMedium color:NSColor.secondaryLabelColor];
    if (gHeader.delta) {
        NSString *gain = gainText;
        NSTextField *field = label_field(gain, [NSFont monospacedDigitSystemFontOfSize:14 weight:NSFontWeightSemibold], accentInk, 1);
        CGFloat available = MAX(0, 244 - x - 51);
        field.frame = NSMakeRect(x + 51, 39, available, 19);
        field.alignment = NSTextAlignmentRight;
        [self addSubview:field];
    }
    // Until a minute of speech gives a speed, the level row moves up into its place.
    BOOL hasSpeed = gHeader.wpm >= 0;
    CGFloat top = hasSpeed ? 0 : kHeaderSpeedHeight;
    double scale = MAX(130, gHeader.wpm);
    for (int row = 0; hasSpeed && row < 2; row++) {
        CGFloat y = 77 + row * 20;
        [self text:row ? @"Typing" : @"You" x:8 y:y width:42 size:11 weight:NSFontWeightRegular color:NSColor.secondaryLabelColor];
        CALayer *track = [CALayer layer];
        track.frame = CGRectMake(52, y+4, 132, 8);
        track.cornerRadius = 4;
        [track setValue:[NSColor.labelColor colorWithAlphaComponent:.07] forKey:@"seeBackground"];
        track.backgroundColor = [NSColor.labelColor colorWithAlphaComponent:.07].CGColor;
        [self.layer addSublayer:track];
        CALayer *fill = [CALayer layer];
        fill.anchorPoint = CGPointMake(0, .5);
        double width = 132 * (row ? 40 : MAX(0, gHeader.wpm)) / scale;
        fill.bounds = CGRectMake(0, 0, width, 8);
        fill.position = CGPointMake(0, 4);
        fill.cornerRadius = 4;
        [fill setValue:(row ? [NSColor.labelColor colorWithAlphaComponent:.25] : accent) forKey:@"seeBackground"];
        fill.backgroundColor = (row ? [NSColor.labelColor colorWithAlphaComponent:.25] : accent).CGColor;
        [track addSublayer:fill];
        if (!row && motion) {
            CABasicAnimation *grow = [CABasicAnimation animationWithKeyPath:@"transform.scale.x"];
            grow.fromValue = @0;
            grow.toValue = @1;
            grow.duration = .5;
            grow.timingFunction = [CAMediaTimingFunction functionWithName:kCAMediaTimingFunctionEaseOut];
            [fill addAnimation:grow forKey:@"grow"];
        }
        [self text:row ? @"40 wpm" : [NSString stringWithFormat:@"%.0f wpm", gHeader.wpm] x:188 y:y width:58 size:11 weight:NSFontWeightMedium color:row ? NSColor.secondaryLabelColor : accentInk];
    }
    if (hasSpeed) {
        [self text:[NSString stringWithFormat:@"%.1fx faster than typing", gHeader.times_typing] x:8 y:124 width:136 size:11 weight:NSFontWeightRegular color:NSColor.secondaryLabelColor];
    }
    if (gSavedLabel.length) {
        NSTextField *saved = label_field(gSavedLabel, [NSFont monospacedDigitSystemFontOfSize:11 weight:NSFontWeightRegular], NSColor.secondaryLabelColor, 1);
        saved.alignment = NSTextAlignmentRight;
        saved.frame = NSMakeRect(140, 124, 106, 16);
        [self addSubview:saved];
    }

    CGPathRef circle = CGPathCreateWithEllipseInRect(CGRectMake(0, 0, 32, 32), NULL);
    for (int i = 0; i < 2; i++) {
        CAShapeLayer *ring = [CAShapeLayer layer];
        ring.frame = CGRectMake(10, 151 - top, 32, 32);
        ring.path = circle;
        ring.fillColor = NSColor.clearColor.CGColor;
        [ring setValue:(i ? accentInk : [NSColor.labelColor colorWithAlphaComponent:.12]) forKey:@"seeStroke"];
        ring.strokeColor = (i ? accentInk : [NSColor.labelColor colorWithAlphaComponent:.12]).CGColor;
        ring.lineWidth = 2.5;
        ring.strokeEnd = i ? gHeader.progress : 1;
        // Start progress at twelve o'clock in the flipped header.
        ring.transform = CATransform3DMakeRotation(-M_PI_2, 0, 0, 1);
        [self.layer addSublayer:ring];
    }
    CGPathRelease(circle);
    NSArray *symbols = @[@"circle.dotted", @"doc.text", @"doc.on.doc", @"book.closed", @"book", @"book.closed.fill", @"books.vertical", @"books.vertical.fill"];
    NSImageView *badge = [NSImageView imageViewWithImage:[NSImage imageWithSystemSymbolName:symbols[MIN(7, MAX(0, gHeader.level_index))] accessibilityDescription:nil]];
    badge.frame = NSMakeRect(17, 158 - top, 18, 18);
    badge.contentTintColor = NSColor.labelColor;
    [self addSubview:badge];
    [self text:gLevelTitle x:54 y:150 - top width:185 size:12 weight:NSFontWeightSemibold color:NSColor.labelColor];
    [self text:gLevelDetail x:54 y:168 - top width:185 size:11 weight:NSFontWeightRegular color:NSColor.secondaryLabelColor];
    NSBox *line = [[NSBox alloc] initWithFrame:NSMakeRect(6, 196 - top, 240, 1)];
    line.boxType = NSBoxSeparator;
    [self addSubview:line];
    [self setAccessibilityElement:YES];
    [self setAccessibilityRole:NSAccessibilityGroupRole];
    NSString *speed = hasSpeed ? [NSString stringWithFormat:@"%.0f words per minute, %.1f times typing speed", gHeader.wpm, gHeader.times_typing] : @"";
    NSMutableArray<NSString *> *parts = [NSMutableArray array];
    for (NSString *part in @[gSinceLabel, [NSString stringWithFormat:@"%@ words", header_number(gHeader.total)],
                             gTodayLabel, speed, gSavedLabel, gLevelTitle, gLevelDetail]) {
        if (part.length) [parts addObject:part];
    }
    [self setAccessibilityLabel:[parts componentsJoinedByString:@". "]];
    [CATransaction commit];
}
@end

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
        case SeeRowLine: return kItemHeight;
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

    BOOL header = body == gBody && gHeader.present;
    CGFloat headerHeight = gHeader.wpm >= 0 ? kHeaderHeight : kHeaderHeight - kHeaderSpeedHeight;
    if (header) height += headerHeight;
    body.frame = NSMakeRect(0, 0, width, height);
    static SeeMenuHeaderView *cachedHeader;
    static NSString *cachedValues;
    NSString *values = [NSString stringWithFormat:@"%@/%@/%@/%@/%@/%@/%@/%@", @(gHeader.total), @(gHeader.previous), @(gHeader.delta), gSinceLabel, gTodayLabel, gSavedLabel, gLevelTitle, gLevelDetail];
    values = [values stringByAppendingFormat:@"/%@/%@/%@", @(gHeader.wpm), @(gHeader.times_typing), @(gHeader.progress)];
    values = [values stringByAppendingFormat:@"/%d", gHeader.level_index];
    BOOL reuse = !gOpeningHeader && [cachedValues isEqualToString:values];
    [body.subviews makeObjectsPerformSelector:@selector(removeFromSuperview)];
    CGFloat y = height - kPadding;
    if (header) {
        NSRect headerFrame = NSMakeRect(kPadding, y - headerHeight, width - kPadding * 2, headerHeight);
        SeeMenuHeaderView *view = reuse ? cachedHeader : [[SeeMenuHeaderView alloc] initWithFrame:headerFrame];
        view.frame = headerFrame;
        cachedHeader = view;
        cachedValues = values;
        [body addSubview:view];
        if (!reuse) [view build:gOpeningHeader];
        y -= headerHeight;
    }
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
            NSFont *font = [NSFont systemFontOfSize:11 weight:NSFontWeightRegular];
            field = label_field(text, font, NSColor.tertiaryLabelColor, 1);
            NSRange globe = [text rangeOfString:@"🌐"];
            if (globe.location != NSNotFound) {
                NSImageSymbolConfiguration *config = [[NSImageSymbolConfiguration
                    configurationWithPointSize:11 weight:NSFontWeightRegular]
                    configurationByApplyingConfiguration:[NSImageSymbolConfiguration
                        configurationWithPaletteColors:@[NSColor.secondaryLabelColor]]];
                NSTextAttachment *key = [[NSTextAttachment alloc] init];
                key.image = [[NSImage imageWithSystemSymbolName:@"globe" accessibilityDescription:@"Globe"]
                    imageWithSymbolConfiguration:config];
                NSMutableAttributedString *styled = [[NSMutableAttributedString alloc]
                    initWithString:text
                        attributes:@{NSFontAttributeName : font,
                                     NSForegroundColorAttributeName : NSColor.tertiaryLabelColor}];
                [styled replaceCharactersInRange:globe
                            withAttributedString:[NSAttributedString attributedStringWithAttachment:key]];
                field.attributedStringValue = styled;
            }
        } else {
            field = label_field(text, item_font(), NSColor.labelColor,
                                row->kind == SeeRowLine ? 1 : kMaxItemLines);
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
#if defined(__MAC_27_0)
        if (@available(macOS 27.0, *)) {
            glass.effectIsInteractive = YES;
        }
#endif
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
#if defined(__MAC_27_0)
        if (@available(macOS 27.0, *)) glass.effectIsInteractive = YES;
#endif
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

static NSString *gSubmenuSource = nil;

/// Beside the row that opened it, kept on screen.
static void place_submenu(void) {
    SeeMenuRowView *source = nil;
    for (NSView *candidate in gBody.subviews) {
        if ([candidate isKindOfClass:SeeMenuRowView.class] &&
            [((SeeMenuRowView *)candidate).rowId isEqualToString:gSubmenuSource]) {
            source = (SeeMenuRowView *)candidate;
            break;
        }
    }
    CGFloat height = NSHeight(gSubmenuPanel.frame);
    NSRect anchor = source ? [source convertRect:source.bounds toView:nil] : gPanel.contentView.bounds;
    anchor = [gPanel convertRectToScreen:anchor];
    NSScreen *screen = gPanel.screen ?: NSScreen.mainScreen;
    NSRect visible = screen.visibleFrame;
    CGFloat x = NSMaxX(gPanel.frame) + 4;
    if (x + kSubmenuWidth > NSMaxX(visible) - 8) x = NSMinX(gPanel.frame) - kSubmenuWidth - 4;
    CGFloat y = NSMaxY(anchor) - height + kPadding;
    y = MAX(NSMinY(visible) + 8, MIN(y, NSMaxY(visible) - height - 8));
    [gSubmenuPanel setFrameOrigin:NSMakePoint(x, y)];
}

void see_menu_show_submenu(const SeeMenuRow *rows, int count, const char *sourceId) {
    if (!gPanel || !gPanel.isVisible || !sourceId) return;
    ensure_submenu_panel();
    CGFloat height = fill_body(gSubmenuBody, kSubmenuWidth, rows, count);
    [gSubmenuPanel setContentSize:NSMakeSize(kSubmenuWidth, height)];
    gSubmenuSource = @(sourceId);
    place_submenu();
    [gSubmenuPanel orderFrontRegardless];
}

// For changes the user did not click: the main rows redraw under an open
// submenu, which stays open and follows its row.
void see_menu_refresh(const SeeMenuRow *rows, int count) {
    if (!gPanel || !gPanel.isVisible) return;
    NSRect frame = gPanel.frame;
    CGFloat height = fill_body(gBody, kPanelWidth, rows, count);
    [gPanel setContentSize:NSMakeSize(kPanelWidth, height)];
    [gPanel setFrameOrigin:NSMakePoint(NSMinX(frame), NSMaxY(frame) - height)];
    if (gSubmenuPanel.isVisible) place_submenu();
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
    gOpeningHeader = YES;
    CGFloat height = fill_body(gBody, kPanelWidth, rows, count);
    [gPanel setContentSize:NSMakeSize(kPanelWidth, height)];

    gOpeningHeader = NO;
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
