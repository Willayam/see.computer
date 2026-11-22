#import <CoreGraphics/CoreGraphics.h>
#import <CoreText/CoreText.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Vision/Vision.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

static NSString *Recognize(CGImageRef image) {
    if (!image) return nil;
    VNRecognizeTextRequest *request = [VNRecognizeTextRequest new];
    request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
    request.usesLanguageCorrection = NO;
    request.recognitionLanguages = @[@"en-US", @"sv-SE"];
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithCGImage:image options:@{}];
    if (![handler performRequests:@[request] error:nil]) return nil;

    NSArray *observations = [request.results sortedArrayUsingComparator:
        ^NSComparisonResult(VNRecognizedTextObservation *a, VNRecognizedTextObservation *b) {
            CGFloat ay = CGRectGetMidY(a.boundingBox), by = CGRectGetMidY(b.boundingBox);
            if (ay != by) return ay > by ? NSOrderedAscending : NSOrderedDescending;
            CGFloat ax = CGRectGetMinX(a.boundingBox), bx = CGRectGetMinX(b.boundingBox);
            if (ax == bx) return NSOrderedSame;
            return ax < bx ? NSOrderedAscending : NSOrderedDescending;
        }];
    NSMutableArray<NSString *> *lines = [NSMutableArray array];
    for (VNRecognizedTextObservation *observation in observations) {
        NSString *line = [observation topCandidates:1].firstObject.string;
        if (line.length) [lines addObject:line];
    }
    return [lines componentsJoinedByString:@"\n"];
}

static char *CopyJSON(NSArray *items) {
    if (!items) return NULL;
    NSData *data = [NSJSONSerialization dataWithJSONObject:items options:0 error:nil];
    NSString *json = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    return json ? strdup(json.UTF8String) : NULL;
}

char *see_ocr_file(const char *path) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithFileSystemRepresentation:path isDirectory:NO relativeToURL:nil];
        CGImageSourceRef source = CGImageSourceCreateWithURL((__bridge CFURLRef)url, NULL);
        if (!source) return NULL;
        CGImageRef image = CGImageSourceCreateImageAtIndex(source, 0, NULL);
        CFRelease(source);
        if (!image) return NULL;
        NSString *text = Recognize(image);
        CGImageRelease(image);
        return text ? strdup(text.UTF8String) : NULL;
    }
}

bool see_ocr_warm(void) {
    @autoreleasepool {
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGContextRef context = CGBitmapContextCreate(NULL, 128, 64, 8, 0, space,
                                                     (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
        CGColorSpaceRelease(space);
        if (!context) return false;
        CGContextSetRGBFillColor(context, 1, 1, 1, 1);
        CGContextFillRect(context, CGRectMake(0, 0, 128, 64));
        CTFontRef font = CTFontCreateWithName(CFSTR("Helvetica"), 24, NULL);
        NSAttributedString *label = [[NSAttributedString alloc] initWithString:@"Vision"
            attributes:@{(__bridge NSString *)kCTFontAttributeName: (__bridge id)font}];
        CFRelease(font);
        CTLineRef line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)label);
        CGContextSetTextPosition(context, 8, 24);
        CTLineDraw(line, context);
        CFRelease(line);
        CGImageRef image = CGBitmapContextCreateImage(context);
        CGContextRelease(context);
        if (!image) return false;
        NSString *text = Recognize(image);
        CGImageRelease(image);
        return text != nil;
    }
}

char *see_window_titles(void) {
    @autoreleasepool {
        NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
        NSMutableArray *titles = [NSMutableArray array];
        for (NSDictionary *window in windows) {
            if ([window[(__bridge NSString *)kCGWindowLayer] integerValue] != 0) continue;
            NSString *owner = window[(__bridge NSString *)kCGWindowOwnerName] ?: @"";
            NSString *title = window[(__bridge NSString *)kCGWindowName] ?: @"";
            [titles addObject:@[owner, title]];
        }
        return CopyJSON(titles);
    }
}

static void RecognizeDisplays(SCShareableContent *content, void (^complete)(NSArray *)) {
    dispatch_group_t group = dispatch_group_create();
    NSMutableArray *texts = [NSMutableArray array];
    for (NSUInteger index = 0; index < content.displays.count; index++) [texts addObject:[NSNull null]];
    [content.displays enumerateObjectsUsingBlock:^(SCDisplay *display, NSUInteger index, BOOL *stop __unused) {
        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
        SCStreamConfiguration *config = [SCStreamConfiguration new];
        config.width = lround(display.width * filter.pointPixelScale);
        config.height = lround(display.height * filter.pointPixelScale);
        config.showsCursor = NO;
        dispatch_group_enter(group);
        [SCScreenshotManager captureImageWithFilter:filter configuration:config
                                  completionHandler:^(CGImageRef image, NSError *error) {
            if (error || !image) {
                dispatch_group_leave(group);
                return;
            }
            CGImageRetain(image);
            dispatch_group_async(group, dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
                @autoreleasepool {
                    NSString *text = Recognize(image);
                    CGImageRelease(image);
                    @synchronized (texts) {
                        if (text) texts[index] = text;
                    }
                }
            });
            dispatch_group_leave(group);
        }];
    }];
    dispatch_group_notify(group, dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
        complete([texts containsObject:[NSNull null]] ? nil : texts);
    });
}

char *see_ocr_displays(uint32_t timeoutMs) {
    static atomic_bool busy = false;
    @autoreleasepool {
        if (!CGPreflightScreenCaptureAccess() || atomic_exchange(&busy, true)) return NULL;
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        __block NSArray *texts = nil;
        void (^complete)(NSArray *) = ^(NSArray *result) {
            texts = result;
            atomic_store(&busy, false);
            dispatch_semaphore_signal(done);
        };
        [SCShareableContent getShareableContentExcludingDesktopWindows:NO
                                                 onScreenWindowsOnly:YES
                                                   completionHandler:^(SCShareableContent *content, NSError *error) {
            if (error || !content) {
                complete(nil);
                return;
            }
            RecognizeDisplays(content, complete);
        }];
        if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, (int64_t)timeoutMs * NSEC_PER_MSEC))) {
            return NULL;
        }
        return CopyJSON(texts);
    }
}
